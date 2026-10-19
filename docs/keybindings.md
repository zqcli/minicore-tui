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
| `Enter` | Send when the session is idle; steer the running turn when it is busy. A draft that starts with `/` is a command line. |
| `Alt+Enter` | Queue the draft as a follow-up while a turn runs; send it when idle. |
| `Shift+Enter`, `Ctrl+J` | Newline. |
| `Esc` | Interrupt the running turn; when idle, clear the draft. |
| `Ctrl+C` | Clear a non-empty draft; on an empty draft press twice to quit. |
| `Ctrl+D` | Quit when the draft is empty; otherwise delete the character after the cursor. |
| `↑` / `↓` | Move between lines of a multi-line draft; on the first or last line, browse prompt history (100 kept). |
| `Alt+↑` | Take the newest queued follow-up back into an empty draft. |
| `Ctrl+G` | Edit the draft in the `editor` from the TUI config, else `$VISUAL`, else `$EDITOR`. |
| `/` at the start of the draft | Open the command palette. |
| `@` | Open the workspace file picker. |
| `Home` / `End`, `Ctrl+A` / `Ctrl+E` | Start or end of the current line. |
| `←` / `→` | Move by character (`Ctrl+←` / `Ctrl+→` do the same). |
| `Alt+←` / `Alt+→` | Move by word. |
| `Backspace` / `Delete` | Delete before or after the cursor. |
| `Ctrl+W` | Delete the previous word. |
| `Ctrl+U` / `Ctrl+K` | Delete to the start or end of the line. |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo (200 steps). |
| Paste | Insert as one edit. A paste of more than 3 lines or 400 bytes shows as a `[pasted N lines]` (or `[pasted N chars]`) token and expands on send. |

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
| `Ctrl+O` | Expand or collapse all tool cards. |
| `Ctrl+T` | Show or hide reasoning and compaction summaries. |
| `Ctrl+F` | Find in the transcript. |
| `F1` | Help view. |
| `q` or `Esc` | Quit from the fatal screen. |

Any upward scroll anchors the view to the content you are reading; new output
does not move it, and a `↓ new output` pill shows while you are above the
bottom. `Ctrl+End` or scrolling to the bottom resumes following the tail.

## Mouse

| Gesture | Action |
|---|---|
| Wheel | Scroll three rows (the diff or tool view when one is open). |
| Click a tool card's header row | Toggle that card. |
| Click an expanded card's `click for full output` row | Open the tool view. |
| Click a turn-end line | Open the diff view for that turn. |
| Drag | Select transcript text; releasing copies it. |

Clicks do nothing while a picker or view is open.

## Pickers and sheets

Pickers, the approval prompt and find share one bottom sheet. `Esc` closes it
without changing anything. Moving the selection works with `↑` / `↓`, `Tab`, and
`PageUp` / `PageDown` (ten rows; in Find they step through matches).

| Sheet | Keys |
|---|---|
| Command palette | The text after `/` stays in the composer and filters the commands fuzzily. `Enter` runs the highlighted command with the argument typed after its name. `Esc` also drops a draft that is only the command. |
| Sessions | Type to filter by title. `Enter` opens the session. `Ctrl+D` twice deletes the selected one; the open session cannot be deleted. Sessions of this workspace come first, newest first; the open one is marked. |
| Model, Reasoning | `Enter` applies the highlighted entry. There is no text filter. |
| Files (`@`) | Type to filter by path substring (up to 500 workspace files). `Enter` replaces the `@` token with `@path`; with no match the picker stays open. `Esc` removes the `@` token. |
| Find | Type to search (case-insensitive, one match per block: messages, notices, tool details, compaction summaries). `Enter`, `↓`, `Tab` go to the next match; `Shift+Enter`, `↑` to the previous. `Esc` closes and leaves the view where it is. |

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
