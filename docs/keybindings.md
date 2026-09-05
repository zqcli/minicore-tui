# Keys And Slash Commands

The keymap is fixed in `src/keymap.rs`. It is pure and compiled into the
binary; v0.2 has no user keybinding configuration. All key actions become
`AppEvent`s and are applied by `App::update`.

## Global Keys

| Key | Behavior |
|---|---|
| `Ctrl+C` | If the composer has text, clear it. If empty, show a hint; press again within 1 second to request shutdown. |
| `Ctrl+D` | Request shutdown only when the composer is empty and the active session is idle. |
| `F1` | Open Help; press `F1` or `Esc` to close it. |
| `Ctrl+R` | Open the session selector. |
| `Ctrl+L` | Open the model selector; updates the active session at a request boundary, or edits a new-session draft. |
| `Shift+Tab` | Open the reasoning selector from the composer; move to the previous form field in a new-session form; close the reasoning selector. |
| `Ctrl+O` | Toggle all durable tool result previews for the active session. |
| `Ctrl+T` | Show or hide durable reasoning runs. |
| `PageUp` / `PageDown` | Scroll the transcript, or page the focused selector/Help/Logs panel. |
| `Ctrl+Home` / `Ctrl+End` | Jump the transcript to the top or tail. |
| `Home` / `End` | Move to the composer line start/end; outside the composer, jump the transcript to the top/tail. |
| `Esc` | Close an open dock; otherwise cancel the active turn from the composer. |
| `q` | Quit only from Help or the fatal overlay. In the composer it is an ordinary character. |
| Mouse wheel | Scroll the transcript by three rows, or move a selector by one item. |

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
the cancellation action.

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
| `/new` | Open a new-session form. |
| `/resume` / `/sessions` | Open the session selector. |
| `/model` | Open the model selector for a draft or active-session update. |
| `/reasoning` | Open the reasoning selector for a draft or active-session update. |
| `/theme dark` / `/theme light` | Change the local palette; no Agent request. |
| `/clear` | Clear only the local active transcript view and reload it from the Agent; refused while a turn runs. |
| `/help` | Open Help. |
| `/logs` | Open the bounded Agent stderr log panel. |
| `/close [confirm]` | Close the active session; blocked/unsaved/running sessions require `confirm`. |
| `/delete [confirm]` | Delete the active session; destructive state requires `confirm`. |
| `/cancel` | Cancel the active loop with `turn.cancel`; the existing `turn.wait` remains in flight. |
| `/refresh` | Request one `turn.wait` for the active retained `TurnRef`; duplicate waits are suppressed. |
| `/quit` | Request normal Agent shutdown. |

`/cancel` and `/refresh` remain local command entries even when a session is
Blocked or Finishing; ordinary prompt/steer/update submissions remain refused.
The following are deliberately not implemented: `!command`, `@file`,
`/fork`, `/branch`, `/compact`, `/steer`, `/queue`, `/settings`, `/login`,
`/plugin`, and `/mcp`.

## Status And Limits

`Ctrl+C` twice, `/quit`, idle `Ctrl+D`, or `q` in Help/fatal state enters the
same shutdown state machine. A live turn is cancelled only with its exact
`TurnRef`; the TUI then waits for an outcome and reconciles durable history.

Tools run automatically under the Agent. Bash is not sandboxed. The TUI supports
mid-turn steering via `turn.steer`. There is no approval UI, compaction control,
live Bash/PTY output, External Editor, or OSC52 copy.
