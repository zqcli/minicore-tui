# Keys and Commands

The keymap is compiled into the binary; there is no user binding file. Input
goes to the frontmost surface, in this order: the fatal screen, an open picker
or view, a pending approval prompt, then the composer and transcript. Any key
press clears the notices.

`Shift+Enter` and `Alt+Enter` are distinct keys only in terminals that support
the kitty keyboard protocol; `Ctrl+J` always inserts a newline.

## Composer

| Key | Action |
|---|---|
| `Enter` | Send when the session is idle; steer the running turn when it is busy. A draft whose first word names a command (`/model`, `/rename x`) runs it; any other draft, `/usr/bin/env is broken` included, is a prompt. |
| `Alt+Enter` | Queue the draft as a follow-up while a turn runs; send it when idle. |
| `Shift+Enter`, `Ctrl+J` | Newline. |
| `Esc` | Interrupt the running turn; when idle, clear the draft. Closes an open picker or view first. |
| `Ctrl+C` | Clear the draft; a second press within 500 ms quits. |
| `Ctrl+D` | Quit when the draft is empty; otherwise delete the character after the cursor. |
| `↑` / `↓` | Move between lines of a multi-line draft; on the first or last line, browse prompt history. The last 100 prompts, paste chips included, are kept in `history.jsonl` next to the TUI config and survive a restart; a prompt over 32 KiB stays in memory only. |
| `Alt+↑` | Take the newest queued follow-up back into an empty draft. |
| `Ctrl+X` | Copy the last assistant answer, as `/copy` does. |
| `Ctrl+G` | Edit the draft in the `editor` from the TUI config, else `$VISUAL`, else `$EDITOR`. |
| `/` at the start of the draft | Open the command palette. `Enter` runs the highlighted command; with no match it sends the line as a prompt. |
| `@` at the start of a word | Open the workspace file picker. In the middle of a word (`me@example.com`) it is plain text. |
| `Home` / `End`, `Ctrl+A` / `Ctrl+E` | Start or end of the current line. |
| `←` / `→` | Move by character. |
| `Ctrl+←` / `Ctrl+→`, `Alt+←` / `Alt+→` | Move by word. |
| `Backspace` / `Delete` | Delete before or after the cursor. |
| `Ctrl+W`, `Ctrl+Backspace` (terminals that send it as `Ctrl+H` too), `Alt+Backspace` | Delete the previous word. |
| `Alt+D`, `Alt+Delete` | Delete to the end of the next word. |
| `Ctrl+U` / `Ctrl+K` | Delete to the start or end of the line. |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo (200 steps). |
| Paste | Insert as one edit. A paste of more than 10 lines or 1000 characters shows as a `[paste #N +L lines]` (or `[paste #N C chars]`) chip and expands on send; recalling a prompt from history brings the chip back. |

Cursor movement and deletion respect whole graphemes and treat a paste token as
one unit. `Esc` while busy only interrupts; it does not clear the draft.

## Transcript and global keys

| Key | Action |
|---|---|
| `PageUp` / `PageDown` | Scroll one viewport minus two rows. |
| `Ctrl+Home` / `Ctrl+End` | Jump to the top of the loaded history (and load older items), or back to the tail. |
| `Ctrl+↑` / `Ctrl+↓` | Put the previous or next user message at the top; past the last one, follow the tail. |
| `Ctrl+R` | Sessions picker. |
| `Ctrl+L` | Model picker. |
| `Shift+Tab` | Switch to the next reasoning level the model supports. |
| `Ctrl+O` | Expand or collapse all tool cards, thinking and compaction summaries. |
| `Ctrl+T` | Collapse thinking to one `Thinking...` row, or show it again. |
| `Ctrl+F` | Find in the transcript. |
| `F1` | Help view. |
| `q` or `Esc` | Quit from the fatal screen. |

Any upward scroll anchors the view to the content you are reading; new output
does not move it, and a ` ↓ Jump to latest message · End ` label shows while you
are above the bottom. `Ctrl+End` or scrolling to the bottom resumes following the tail.

## Mouse

| Gesture | Action |
|---|---|
| Wheel | Scroll three rows. With a view open it scrolls that view (help, diff, tool output; the context view does not scroll); over a picker it moves the selection. It never scrolls the transcript behind them. |
| Click a tool card's header row | Toggle that card. |
| Click a card's `click for full output` row | Open the tool view. Expanded cards end in this row; so does a card reloaded from history, whose row reads `… N lines · click for full output`. |
| Click a turn-end line | Open the diff view for that turn. |
| Drag | Select transcript text; releasing copies it. |

Clicks do nothing while a picker or view is open.

## Pickers and sheets

Pickers, the approval prompt and find share one bottom sheet that takes the
editor's place. `Esc` closes it without changing anything. `↑` / `↓` move the
selection and wrap at the ends, as do `Tab`; `PageUp` / `PageDown` move ten
rows (in Find they step through matches). The command and `@` lists are not
sheets: they sit under the editor text, five rows at a time with an `(i/N)`
counter.

| Sheet | Keys |
|---|---|
| Command list | The text after `/` stays in the composer and filters the commands fuzzily, best match first. `Enter` runs the highlighted command with the argument typed after its name; `Tab` completes `/name ` without running. `Esc` closes the list and leaves the draft as it is; deleting the `/` closes it too. |
| Sessions | Type to filter by title. `Enter` opens the session. `Ctrl+D` twice deletes the selected one; the open session cannot be deleted. Sessions of this workspace come first, newest first; the open one is marked. |
| Model | Type to filter by id. `Enter` switches to the highlighted model; the current one is marked `✓`. |
| Reasoning | `Enter` applies the highlighted level; the current one is marked `✓`. |
| Files (`@`) | The `@word` before the cursor filters workspace paths (up to 500 files). `Enter` or `Tab` replaces it with `@path `; with no match `Enter` does nothing. `Esc` closes the list and leaves the text. |
| Find | Type to search (case-insensitive, one match per block: messages, notices, tool details, compaction summaries); the view shows match 1 as you type. `Enter`, `↓`, `Tab` go to the next match; `Shift+Enter`, `↑` to the previous. `Esc` closes and leaves the view where it is. |

## Approval and choice prompts

While an interaction is pending, only these keys act; the rest are ignored.

| Key | Action |
|---|---|
| `y` | Allow once (approval). |
| `n`, `Esc` | Deny (approval). |
| `↑` / `↓`, `Enter` | Select a row and submit it. For an approval the first row is `Allow once`, the second `Deny`. |

## Views

`Esc` returns from a view to the transcript with its scroll position intact.

| View | Keys |
|---|---|
| Diff | `Tab` cycles the scope (turn, session, workspace); `↑` / `↓` select a file; `PageUp` / `PageDown`, `Home` / `End` and the wheel scroll the diff; `c` copies it. |
| Tool output | `[` `]` or `←` `→` show the previous or next tool call; `↑` / `↓` scroll a row; `PageUp` / `PageDown`, `Home` / `End` and the wheel scroll; `c` copies the output. |
| Context | `r` re-reads the context (it also reads on open); `c` copies it. |
| Help | `↑` / `↓` scroll three rows, `PageUp` / `PageDown` thirty. |

## Commands

Type `/` for the palette, or the whole line and press `Enter`. Arguments follow
the name.

| Command | Effect |
|---|---|
| `/new` | Create a session with the current defaults. |
| `/sessions`, `/resume` | Open the sessions picker. |
| `/rename <title>` | Rename the current session (an empty title is an error). |
| `/model` | Open the model picker. |
| `/reasoning [level]` | Set the level directly, or open the picker when no valid level is given. |
| `/compact` | Run manual compaction; refused while a turn runs. |
| `/diff [session\|workspace]` | Open the diff view for the last turn, or the given scope. |
| `/tool` | Open the full output of the tool card at the top of the view, else the newest. |
| `/context` | Open the context view. |
| `/copy` | Copy the last assistant answer as Markdown. |
| `/export [path]` | Write the conversation as Markdown; the default is `./minicore-<session>-<date>.md`. |
| `/theme [dark\|light]` | Switch the palette and save it; without an argument, toggle. |
| `/reload` | Ask the Agent to reload its configuration. |
| `/help` | Open the help view. |
| `/quit` | Quit. |
