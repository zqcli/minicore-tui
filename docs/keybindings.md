# Keys and Commands

The keymap is compiled into the binary; there is no user binding file. Keys act
on the focused surface: a full-screen view, a bottom-sheet panel, or the
transcript and composer.

## Composer

| Key | Action |
|---|---|
| `Enter` | Send when the session is idle; steer when a turn is running. |
| `Alt+Enter` | Newline when idle; queue a follow-up when a turn is running. |
| `Shift+Enter`, `Ctrl+J` | Newline. |
| `Esc` | Close a panel or view, then clear a selection, then interrupt the running turn. |
| `Ctrl+C` | Clear a non-empty composer; press twice on an empty composer to quit. |
| `Ctrl+D` | Quit when the composer is empty. |
| `↑` / `↓` | Move inside a multi-line draft; at the first or last line, browse prompt history. |
| `Alt+↑` | Pull the last queued message back into the composer. |
| `Ctrl+G` | Edit the draft in `$VISUAL`, `$EDITOR` or the configured editor. |
| `/` at the start of the draft | Open the command palette. |
| `@` | Open the workspace file picker and insert the chosen path. |
| `Ctrl+A` / `Ctrl+E` | Move to the start or end of the current line. |
| `Alt+←` / `Alt+→`, `Ctrl+←` / `Ctrl+→` | Move by word. |
| `Ctrl+W` | Delete the previous word. |
| `Ctrl+U` / `Ctrl+K` | Delete to the line start or end. |
| `Ctrl+Z`, `Ctrl+Shift+Z` or `Ctrl+Y` | Undo, redo. |
| Bracketed paste | Insert the paste as one edit. Multi-line pastes collapse to a `[pasted N lines]` token and expand on send. |

Cursor movement and deletion respect complete graphemes. Up and Down move
between logical lines; soft wraps do not change where the cursor stops.

## Transcript

| Key | Action |
|---|---|
| `PageUp` / `PageDown` | Scroll one viewport minus two rows. |
| Wheel | Scroll three rows. |
| `Ctrl+Home` / `Ctrl+End` | Jump to the top or the tail. |
| `Ctrl+↑` / `Ctrl+↓` | Jump to the previous or next user message. |
| Click a tool card header | Toggle that card. |
| Click an expanded card's `click for full output` row | Open the tool view. |
| Click a turn-end line | Open the diff view scoped to that turn. |
| Drag | Select transcript text; release copies it. |

Any upward scroll anchors the view to the content you are reading; new output
does not move it. `Ctrl+End` or scrolling to the bottom resumes following the
tail. While anchored, a `↓ new output` pill appears when content is appended.

## Global

| Key | Action |
|---|---|
| `Ctrl+R` | Sessions picker. |
| `Ctrl+L` | Model picker. |
| `Shift+Tab` | Cycle the reasoning level among the levels the model supports. |
| `Ctrl+O` | Expand or collapse all tool cards. |
| `Ctrl+T` | Show or hide reasoning. |
| `Ctrl+F` | Find in the transcript. |
| `F1` | Help. |
| `Esc` | Close the frontmost surface. |
| `q` | Quit from the fatal screen. |

## Pickers and Panels

All pickers use the same bottom sheet: a title row, a highlighted selection
band and a dim hint row.

| Key | Action |
|---|---|
| Typing | Filter the list. |
| `↑` / `↓`, `PageUp` / `PageDown` | Move the selection. |
| `Enter` | Open the selected entry or run the selected command. |
| `Esc` | Close the panel without changing anything. |

The sessions picker lists sessions for the workspace with the current one
first, showing title or first prompt, model and age. `Ctrl+D` twice deletes the
selected session; a loaded session must be closed and idle first.

The command palette shows each command with a one-line description and filters
fuzzily. The file picker uses the Agent's workspace listing and search, and
inserts the selected path into the draft.

## Find

`Ctrl+F` opens an inline find bar over the transcript.

| Key | Action |
|---|---|
| Typing | Search as you type. |
| `Enter` | Next match. |
| `Shift+Enter` | Previous match. |
| `Esc` | Close the bar and restore the previous scroll position. |

## Views

`Esc` returns from a view to the transcript with its scroll position intact.

| View | Keys |
|---|---|
| Diff | `Tab` cycles turn / session / workspace scope; `↑`/`↓` select a file; `PageUp`/`PageDown` scroll the diff; `c` copies the diff. |
| Tool output | `[` / `]` show the previous or next tool call; `PageUp`/`PageDown` scroll; `c` copies the output; `Esc` returns. |
| Context | `r` re-reads the context; `Esc` returns. It also re-reads each time it opens. |
| Help | `PageUp`/`PageDown` scroll; `Esc` returns. |

## Interaction Prompts

An approval or other interaction request opens a sheet above the composer.

| Kind | Keys |
|---|---|
| Approval | `y` allows once, `n` denies, or select and press `Enter`. |
| Choice | `↑`/`↓` select, `Enter` submits. |

## Commands

| Command | Effect |
|---|---|
| `/new` | Create a session with the current defaults. |
| `/sessions`, `/resume` | Open the sessions picker. |
| `/rename <title>` | Rename the current session. |
| `/model` | Open the model picker. |
| `/reasoning` | Open the reasoning picker. |
| `/compact` | Run manual compaction when the session is idle. |
| `/diff` | Open the diff view for the current turn. |
| `/context` | Open the context view. |
| `/copy` | Copy the last assistant answer as Markdown. |
| `/export [path]` | Write the conversation as Markdown. |
| `/theme` | Toggle the palette and persist it. |
| `/reload` | Reload the Agent configuration. |
| `/help` | Open the help view. |
| `/quit` | Quit. |
