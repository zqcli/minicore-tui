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
| `Enter` | Send when the session is idle; steer the running turn when it is busy. A draft whose first word names a command (`/model`, `/rename x`) or a prompt template runs it. A bare `/word` that names nothing (`/modle`) is not sent: a notice offers the nearest command, and a second `Enter` (or a leading `//`, which sends the rest) sends it as text. Any other draft, `/usr/bin/env is broken` and `/usr/bin` included, is a prompt. |
| `Alt+Enter` | Queue the draft as a follow-up while a turn runs; send it when idle. |
| `Shift+Enter`, `Ctrl+J` | Newline. |
| `Esc` | Close an open picker or view first; in the editor, cancel an active selection before interrupting a turn, cancelling a compaction or clearing the draft. |
| `Ctrl+C` | Copy the editor selection when one exists; otherwise clear the draft, with a second press within 500 ms quitting. |
| `Ctrl+D` | Quit when the draft is empty; otherwise delete the character after the cursor. |
| `↑` / `↓` | Move between lines of a multi-line draft; on the first or last line, browse prompt history. The last 100 prompts, paste chips included, are kept in `history.jsonl` in the TUI directory and survive a restart; a prompt over 32 KiB stays in memory only. |
| `Alt+↑` | Take the queued follow-ups back into an empty draft, separated by blank lines. |
| `Ctrl+X` | Cut the editor selection when one exists; otherwise copy the last assistant answer, as `/copy` does. |
| `Ctrl+G` | Edit the draft in `[ui] editor` from `config.toml`, else `$VISUAL`, else `$EDITOR`. |
| `/` at the start of the draft | Open the command palette. `Enter` runs the highlighted command; with no match it sends the line as a prompt. |
| `@` at the start of a word | Open the workspace file picker. In the middle of a word (`me@example.com`) it is plain text. Before the first message there is no session to list files from: `@` stays text and a notice says so. |
| `Home` / `End`, `Ctrl+A` / `Ctrl+E` | Start or end of the current line (`Home` with an empty draft and `End` also scroll, see below). |
| `←` / `→` | Move by character. |
| `Ctrl+←` / `Ctrl+→`, `Alt+←` / `Alt+→` | Move by word. |
| `Backspace` / `Delete` | Delete the selection, or the character before or after the cursor. |
| `Ctrl+W`, `Ctrl+Backspace` (terminals that send it as `Ctrl+H` too), `Alt+Backspace` | Delete the previous word. |
| `Alt+D`, `Alt+Delete` | Delete to the end of the next word. |
| `Ctrl+U` / `Ctrl+K` | Delete to the start or end of the line. |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo (200 steps). |
| Paste | Insert as one edit, replacing the selection. Newlines, including a trailing newline, are draft text rather than send commands. A paste of more than 10 lines or 1000 characters shows as a `[paste #N +L lines]` (or `[paste #N C chars]`) chip and expands on send; recalling a prompt from history brings the chip back. |

Hold `Shift` with Left/Right, Up/Down, Home/End to extend or shrink a selection;
Ctrl/Alt+Shift+Left/Right extends by word. Typing, pasting or inserting a newline
replaces the selection. Plain Left/Right collapses it to the corresponding end.
Edits and selection replacements support undo/redo.

Cursor movement and deletion respect whole graphemes and treat a paste token as
one unit. With no selection, `Esc` while busy only interrupts; it does not clear
the draft. Copying or cutting a selection drops it, so `Ctrl+C` can quit again;
a cut too large for the clipboard is refused and leaves the text. With no draft,
`Shift+Home` and `Shift+End` scroll like `Home` and `End`. Selecting in a
recalled prompt keeps the draft that `↓` returns to. The editor requests a
non-blinking block cursor and restores the terminal default on exit or
external-editor handoff.

On Windows the console reports a bracketed paste as its keys, so the TUI decodes
the markers itself; a paste whose end marker does not come within a second is
taken as it is. Unbracketed text that ends with Enter and arrives within 30 ms
counts as one paste, which delays typed keys by that much on Windows only. A
paste slower than that is indistinguishable from typing.

## Before the first message

With no session open (a start without `--session` or `--continue`, or after
`/new`) the screen shows the welcome header over an empty transcript and no
session exists; the header stays at the top of the new session. `Enter` or `Alt+Enter`
with text creates the session with the profile, model and reasoning chosen so
far and sends the text as its first message; if the creation fails the text is
back in the editor with the error. While the session is being created another
`Enter` leaves its text in the editor. `/model`, `/reasoning`, `Ctrl+L`,
`Ctrl+P` and `Shift+Tab` choose the settings of that session and confirm with a
notice, and `/new <profile>` chooses its profile. `Ctrl+R` opens a stored
session. Commands that need a session (`/compact`, `/context`, `/diff`,
`/tool`, `/rename`, `/export`, `/copy`, `/session`) answer
`No session yet: send a message first`. Quitting creates nothing.

## Transcript and global keys

| Key | Action |
|---|---|
| `PageUp` / `PageDown` | Scroll one viewport minus four rows. |
| `Ctrl+Home`, or `Home` with an empty draft | Jump to the top of the loaded history and load older items. |
| `End` | Move to the end of the line and follow the tail again. |
| `Ctrl+↑` / `Ctrl+↓` | Put the previous or next user message at the top; past the last one, follow the tail. |
| `Ctrl+R` | Sessions picker. |
| `Ctrl+L` | Model picker. |
| `Ctrl+P` / `Shift+Ctrl+P` | Switch to the next or previous model in the catalog (`Shift+Ctrl+P` needs the kitty keyboard protocol; other terminals send `Ctrl+P` for both). |
| `Shift+Tab` | Switch to the next reasoning level the model supports. |
| `Ctrl+O` | Expand or collapse all tool cards, thinking and compaction summaries. The top of the view stays put, so blocks open and close downward. |
| `Ctrl+T` | Collapse thinking to one `Thinking...` row, or show it again; the top of the view stays put. |
| `Ctrl+F` | Find in the transcript. |
| `F1` | Help view. |
| `q` or `Esc` | Quit from the fatal screen. |

Any upward scroll anchors the view to the content you are reading; new output
does not move it, and a ` ↓ Jump to latest message · End ` label shows while you
are above the bottom. `End` or scrolling to the bottom resumes following the tail.

## Mouse

| Gesture | Action |
|---|---|
| Wheel | Scroll one row per notch, five with `Alt`. With a view open it scrolls that view (help three rows per notch, diff, tool output; the context view does not scroll); over a picker it moves the selection. It never scrolls the transcript behind them. |
| Click a folded tool card's text, or any row of an expanded card but its `click for full output` row | Toggle that card. It opens and closes downward: the rows above it stay put, and `End` follows the tail again. Dragging over a card selects instead. |
| Click an expanded card's `click for full output` row | Open the tool view. The row is always the first under the title or command; it reads `… N earlier lines · click for full output` when lines are hidden (`… N lines · …` on a card reloaded from history) and just `click for full output` otherwise. |
| Click `↑ top` or `↓ bottom` in the tool view's title row | Scroll the output to its first or last row. |
| Click a turn-end line | Open the diff view for that turn. |
| Click in the editor | Place the caret at a grapheme or paste-chip boundary, respecting wrapping, padding and editor scrolling. |
| Drag in the editor | Select draft text; releasing copies it and keeps it highlighted. Typing replaces it. Moving outside the editor reaches adjacent rows and scrolls the draft. |
| Shift+click in the editor | Extend the selection from its fixed anchor. Many terminals keep Shift+click for their own selection and never send it. |
| Drag in the transcript | Select transcript text; releasing copies it. Only content is taken: rails, padding, card durations, expand hints, timestamps, turn-end lines and the welcome header are neither highlighted nor copied. |

Editor clicks are accepted while an inline command or file completion list is
open, but completion rows are not editor text. Full-screen views, approval
prompts and bottom sheets do not expose the hidden draft for mouse editing.

## Pickers and sheets

Pickers, the approval prompt and find share one bottom sheet that takes the
editor's place. `Esc` closes it without changing anything. `↑` / `↓` move the
selection and wrap at the ends, as do `Tab`; `PageUp` / `PageDown` move ten
rows (in Find they step through matches). The command and `@` lists are not
sheets: they sit under the editor text, five rows at a time with an `(i/N)`
counter.

| Sheet | Keys |
|---|---|
| Command list | The text after `/` stays in the composer and filters the commands fuzzily, best match first. `Enter` runs the highlighted command with the argument typed after its name; `Tab` completes `/name ` without running. After `/model `, `/new `, `/reasoning `, `/theme ` or `/diff ` the list offers that command's arguments (model ids, profiles, levels) and `Enter` runs the command with the highlighted one. Prompt templates are listed with their description. `Esc` closes the list and leaves the draft as it is; deleting the `/` closes it too. |
| Sessions | Type to filter by title. `Enter` opens the session. `Ctrl+N` renames the selected one (`Enter` saves, `Esc` cancels; an empty title clears it). `Ctrl+D` twice deletes the selected one; the open session cannot be deleted. Sessions of this workspace come first, newest first. Each row ends with its last activity; the open one says `current`, one from another workspace names it, and a title shared with another row adds the short id. |
| Session (`/session`) | Id, title, workspace, profile, model, history items and token totals. `Enter` or `c` copies the id. |
| Model | Type to filter by id. `Enter` switches to the highlighted model; the current one is marked `✓`. |
| Reasoning | `Enter` applies the highlighted level; the current one is marked `✓`. |
| Files (`@`) | The `@word` before the cursor filters workspace paths (up to 500 files). `Enter` or `Tab` replaces it with `@path `; with no match `Enter` does nothing. `Esc` closes the list and leaves the text. |
| Find | Type to search (case-insensitive; every occurrence counts: messages, notices, thinking, tool details and loaded tool output, compaction summaries); the view lands on the row of match 1 as you type, opens the folded thinking or tool output that holds it, and highlights the matches on screen. `Enter`, `↓`, `Tab` go to the next match; `Shift+Enter`, `↑` to the previous. `Esc` closes and leaves the view where it is. |

## Approval and choice prompts

While an interaction is pending, only these keys act; the rest are ignored.

| Key | Action |
|---|---|
| `y` | Allow once (approval). |
| `a` | Allow once and answer every later approval of the same tool for the rest of this session (the footer shows `allow <tool>`). |
| `n`, `Esc` | Deny (approval). |
| `↑` / `↓`, `Enter` | Select a row and submit it. For an approval the rows are `Allow once`, `Deny` and `Allow <tool> this session`. An `edit` or patch approval shows its diff. |

## Views

`Esc` returns from a view to the transcript with its scroll position intact.
A view owns the keys: the composer below it is dimmed and reads `view open · esc to return`, and the keys the view does not bind do nothing.

| View | Keys |
|---|---|
| Diff | `Tab` cycles the scope (turn, session, workspace); `↑` / `↓` select a file; `PageUp` / `PageDown`, `Home` / `End` and the wheel scroll the diff; `c` copies it. |
| Tool output | `[` `]` or `←` `→` show the previous or next tool call; `↑` / `↓` scroll a row; `PageUp` / `PageDown` and the wheel scroll; `Home` / `End` jump to the top or bottom; `c` copies the output. When the output does not fit, the title row ends with the visible range (`31–60 of 412`) and the `↑ top` / `↓ bottom` buttons. |
| Context | `r` re-reads the context (it also reads on open); `c` copies it. |
| Help | `↑` / `↓` scroll three rows, `PageUp` / `PageDown` thirty. |

## Commands

Type `/` for the palette, or the whole line and press `Enter`. Arguments follow
the name.

| Command | Effect |
|---|---|
| `/new [profile]` | Leave the open session for an empty conversation; the first message creates the new session, with the Agent's default profile or the named one. |
| `/sessions`, `/resume` | Open the sessions picker. |
| `/rename <title>` | Rename the current session (an empty title is an error). |
| `/model [id]` | Switch to the model (an exact id or a unique fuzzy match), or open the picker. When the model does not support the current reasoning level, the switch also moves to the nearest level it supports (the higher one on a tie) and a notice names it. `Ctrl+P` and `Ctrl+L` do the same, and a draft adjusts the level its session will start with. |
| `/session` | Show the open session's id, workspace, profile, model, item count and token totals. |
| `/reasoning [level]` | Set the level directly, or open the picker when no valid level is given. |
| `/compact` | Run manual compaction; refused while a turn runs. |
| `/diff [session\|workspace]` | Open the diff view for the last turn, or the given scope. |
| `/tool` | Open the full output of the tool card at the top of the view, else the newest. |
| `/context` | Open the context view. |
| `/copy` | Copy the last assistant answer as Markdown. |
| `/export [path]` | Write the conversation as Markdown; the default is `./minicore-<session>-<date>.md`. |
| `/theme [dark\|light]` | Switch the palette and save it; without an argument, toggle. |
| `/reload` | Compose the Agent config again from `config.toml`, `models.toml` and the prompt, reload the Agent and the templates. |
| `/help` | Open the help view. |
| `/quit` | Quit. |

## Prompt templates

Each `*.md` file in `prompt/commands/` of the TUI directory is a command named after the file; they are
read at start and by `/reload`. A template shadows no built-in command. An
optional `---` front matter gives `description:` and `argument-hint:`; without a
description the first line is used. `/name args` expands `$1`, `$2`, …, `$@` (or
`$ARGUMENTS`) and `${1:-default}` from the shell-quoted arguments and sends the
text, as pi does.

## Terminal title, bell and resume hint

The window title is `workspace · session title · idle|working|approval`, written
only when it changes. The terminal bell rings when a turn that ran more than
10 s ends, or asks for approval; `bell = false` under `[ui]` in `config.toml` turns it off.
Quitting restores the title and prints `Resume: minicore-tui --session <id>` for
a session with history.
