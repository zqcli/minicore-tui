# Keys And Slash Commands (v0.3.0)

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
| `F1` | Open Help; `F1` or `Esc` returns to the previous panel with its input intact. |
| `Ctrl+R` | Open the session selector. |
| `Ctrl+G` | In Composer with Editor focus, edit the current draft externally. A browsed closed session keeps explicit Continue without sending; the Session panel also retains Continue. |
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

Help retains the previous form, selector filter, or Logs scroll position once.
Opening another panel from Help replaces that return context. A panel submitting
a session change stays visible until its response arrives, with a notice when
Help is requested; streaming and pending reads do not block Help.

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

`/settings` edits only local TUI preferences: theme, reasoning/tool defaults,
external-editor executable/arguments, and the next-start Agent paths. `Ctrl+S`
applies a valid settings form atomically; an Agent path change is reported as a
next-startup requirement and never reloads or kills the current child.
Settings text fields support Left/Right, Home/End (also Ctrl+A/Ctrl+E),
Backspace/Delete, Ctrl+U clear, and bracketed paste. In Editor args, Enter or
Ctrl+J adds a separate argument; multiline paste preserves argument boundaries.
Esc cancels unsaved edits. A failed write keeps the edited form for retry.
`Ctrl+G` opens the exact current draft, including multiline and Unicode text,
when the Composer's Editor has focus. A browsed closed session keeps `Ctrl+G`
as Continue; return to an active session before using the shortcut for editing.
`/editor` starts with a blank file, never its own command text. Both entries use
the configured executable, its exact argument vector, and an OS temporary file.
Configure the editor in `/settings` or with `MINICORE_TUI_EDITOR`; without one,
the draft is kept and a configuration notice is shown. An unchanged successful
return retains the cursor, undo/redo, and collapsed paste markers. An edited
return replaces the draft without submitting it. The terminal is suspended only for
input/drawing; RPC readers, App updates, background turns, waits, and saves
continue. Invalid UTF-8, oversized output, nonzero exit, cancellation, and a
stale draft preserve the existing Composer text.

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

## Changes / Diff And Context (v0.3 E3)

`/diff` (or `/diff workspace`) opens workspace Changes. `/diff session` and
`/diff turn <loop_id>` request native write/edit/apply_patch records for the
explicit scope. Workspace attribution is unknown; Bash/user/external edits
are not attributed to the Agent's native records. Workspace reads require a
loaded Session; closed sessions require explicit Continue for workspace scope,
never implicit open. Native session/Turn records remain read-only cold reads.

| Main focus/key | Behavior |
|---|---|
| Changes list: Up/Down, Page keys, End, wheel/scrollbar | Move/select/scroll received records; no implicit fetch. |
| Changes list: Enter | Open the selected record's single-file unified diff. |
| Tab / Shift+Tab | List: workspace/session scope. Diff: cycle head_to_worktree, head_to_index, index_to_worktree; native tool_before_after is fixed. |
| Ctrl+N | Read the exact next cursor when available; stale/limit/error never silently rescans. |
| F5 or `/refresh` from Editor | Refresh the current list/comparison plus an explicit status observation, without reloading conversation history. |
| Diff: Page/arrows/wheel/scrollbar, End | Scroll locally / follow the loaded tail. |
| Diff: Ctrl+Shift+C | Copy the immutable layout snapshot's safe line-source text, not hunk/sign/line-number decoration or an applicable patch. Partial/stale/incomplete-line/display-limit data is labelled. |
| Esc | Diff → list → conversation, preserving both positions, draft and conversation anchor. Never cancel a tool. |

`/context` opens coverage, estimates, automatic preparation, manual operation
and utility usage as separate sections. Unknown values are not zero and no
summary body is attached or rendered. Tab selects Refresh / compact / exact
operation cancel; Enter invokes the selected enabled action. F5 or `/refresh`
reads context only. Manual compact uses the existing loaded/idle/settled/
unblocked admission; cancellation never substitutes a guessed TurnRef.
Page/arrows/End and wheel/scrollbar stay local. Closing the page stops its
observation, not a B-owned operation or mandatory confirmation. Idle does not
poll; active foreground/background operation reads are bounded by 500 ms/2 s.

Both pages retain the existing Editor and one-line Footer. F6 switches focus;
Editor keys keep their normal meanings. Dock/selection Esc precedes closing
the main view, and only subsequent explicit cancel acts on execution.
Footer git identity is an explicit last observation: unknown is `git?`, a
confirmed non-repository has no branch suffix, and `seen:` / `stale:` plus
detached state are distinct. It is not a live repository watcher.

## Workspace Files And Literal Search (v0.3 E2)

Type `@` at a word boundary (start or after whitespace), or use `/files [path
filter]`, to open a temporary file Dock. An email `name@host` and a pasted `@`
remain ordinary text. These interfaces require an already loaded Session;
closed-history browsing offers an explicit Continue, never an automatic open.

| Dock key | Behavior |
|---|---|
| Characters / paste / Backspace / Delete / Ctrl+U | Edit or clear the current single-line query/scope; content changes debounce once after 150 ms. |
| Left/Right / Home/End (also Ctrl+A/Ctrl+E) | Move the active input cursor; query and scope keep independent positions. No scan is restarted. |
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

## Search And Export Input

Search query, Export target, and Files/Grep query/scope support Left/Right,
Home/End, Backspace/Delete, and single-line bracketed paste at the cursor.
Long fields scroll horizontally to keep the insertion point visible. Unicode
cursor positions use UTF-8 character boundaries. Ctrl+U clears the active field.

Paste inserts once and never presses Enter, submits a turn, runs a slash command,
or writes an export. Line breaks (LF, CRLF, or CR), NUL, and other control
characters except Tab reject the whole insertion with a notice; the field is
unchanged. Tabs remain literal, displayed as spaces (including JSON whitespace
in Grep paths). Search retains its 262144-byte query bound; Files/Grep retain
1024-byte query and 4096-byte scope bounds. Over-limit insertions are rejected
whole rather than truncated.

Search has two modes: Home/End move the query cursor in Input, but select the
first/last match in Results. Left/Right or typing return to Input; Enter searches
from Input and jumps from Results. Results retain Up/Down, Page keys, n/p and s.
Ctrl+A always switches loaded/full scope; it is never a query-Home shortcut.
A running scan keeps its original literal and cannot take focus from an edited
query. Results for an older literal are explicitly labelled as the previous query. Enter starts a fresh generation for the edited query.

Export keeps Ctrl+Y overwrite and Ctrl+R raw, along with its other existing
option chords. Its field is frozen during writing/cancellation. A failed export
keeps the path, options and outcome available while the path is corrected for
an explicit Enter retry. In Export and Files/Grep, Ctrl+A/Ctrl+E also move the
input cursor to the beginning/end.

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
Their draft text is kept so it can be corrected without retyping.

Typing `/` shows six top-level entries: Model, Reasoning, Session,
Workspace, Conversation and App. Groups are marked `›`; Enter opens a group.
Type after `/` to search every command, or after a group to filter its children.
For example, `/workspace files src` and `/files src` use the same executor.
`/session list` opens Sessions; `/session configure` opens the advanced form.

Up/Down and PageUp/PageDown move selection. Tab only fills text; Enter runs a
complete command. Esc clears a group filter, then goes back to the root, then
hides the menu while keeping the draft. Esc in a visible menu never cancels a
turn. A no-match menu cannot execute a stale selection. Moving within an
existing command or editing multiple lines uses ordinary editor keys rather
than replacing a partial token. Dismissed menus stay hidden until text changes.

Required-argument commands stay editable: `/theme` offers dark/light choices,
while `/tool` shows its exact argument usage. `/model [id]` and
`/reasoning [level]` accept catalog-supported values or open their picker when
no value is given. Model IDs are case-sensitive. A model incompatible with the
current reasoning opens the supported reasoning picker; both choices are sent
atomically only after confirmation. Cancellation makes no configuration request.
Updates take effect at the existing next-request boundary, never retroactively.

| Command | Behavior |
|---|---|
| `/new [form]` | Quickly create here using recent explicit settings; `form` opens the full form. |
| `/resume` / `/sessions` | Continue a browsed session / open the session selector. |
| `/model [id]` | Open the model selector, or choose an exact catalog ID. |
| `/reasoning [level]` | Open the reasoning selector, or choose a supported effort level. |
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
| `/compact` / `/context` | Start manual compaction / open the concrete Context main view over the existing operation owner. |
| `/settings` / `/editor` | Local preferences / start a blank draft in the external editor (`Ctrl+G` edits the current draft). |
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
