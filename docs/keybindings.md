# Keybindings and commands

minicore-tui follows the Pi fullscreen interaction style. The key map is a
fixed table compiled into `src/keymap.rs` (development spec 22.4); there is
no user key configuration. All of this is implemented in Phase 5; the
agent connection itself arrives in Phase 6.

## Global

| Key | Behavior |
|---|---|
| `Ctrl+C` | composer has text: clear it; empty: first press shows a hint, a second press within 1s quits |
| `Ctrl+D` | quit when the composer is empty and the session is idle |
| `F1` | open Help (Esc or F1 closes it) |
| `Ctrl+R` | session selector |
| `Ctrl+L` | model selector (target: a new session) |
| `Shift+Tab` | reasoning selector from the composer; previous field in the new-session form; closes the reasoning selector |
| `Ctrl+O` | expand/collapse all tool cards |
| `Ctrl+T` | show/hide reasoning runs |
| `PageUp` / `PageDown` | scroll the transcript (or page a selector / Help / Logs) |
| `Up` / `Down` (Help/Logs) | scroll those panels by one row |
| `Ctrl+Home` / `Ctrl+End` | transcript top / tail |
| `Esc` | close the open panel; otherwise cancel the running turn |
| `q` | quits only in the Help panel and on the fatal overlay; everywhere else it is an ordinary character |
| mouse wheel | scroll the transcript by 3 rows, or move a selector's selection |

`Home` moves to the transcript top when no editor is focused; inside the
composer it moves to the line start. `End` moves to the line end inside the
composer and to the transcript tail elsewhere (use `Ctrl+Home`/`Ctrl+End`
to jump the transcript while typing).

## Composer

| Key | Behavior |
|---|---|
| Enter | send (or run a `/` command) |
| `Shift+Enter` / `Ctrl+J` | newline |
| `Ctrl+A` / `Ctrl+E` | line start / line end |
| `Ctrl+W` | delete the previous word |
| `Ctrl+Z` / `Ctrl+Y` | undo / redo |
| `Up` (first row) / `Down` (last row) | previous / next history message |
| `Alt+Up` / `Alt+Down` | history navigation (same as the row-edge arrows) |
| Backspace / Delete / arrows / Home / End | standard editing |
| CJK / emoji / combining marks | inserted as characters; the cursor column always uses display cells |

History keeps the last 100 non-empty submitted messages per process and
is not persisted. While a turn is running the composer is frozen: typing,
editing and submitting are disabled and `Esc` cancels the exact turn.
Paste (`bracketed paste`) inserts the whole buffer in one edit, normalizes
`CRLF`/`CR` to `LF`, and never submits by itself.

## Selectors

| Key | Behavior |
|---|---|
| `Up` / `Down` | move the selection (the filter window follows) |
| Enter | confirm |
| `Esc` | cancel back to the form (or composer) |
| printable characters | type into the search query |
| Backspace | remove the last query character |
| `Ctrl+U` | clear the query |
| `PageUp` / `PageDown` | page the selection |
| mouse wheel | move the selection |

The session selector keeps its query and selection when an open fails,
and re-enables itself after the response.

## New-session form

| Key | Behavior |
|---|---|
| `Tab` / `Shift+Tab` | next / previous field |
| Enter | confirm the field: profile/model/reasoning open their selector; Create submits |
| typing | edits the workspace/title field at a char cursor (arrow keys move it) |
| `Ctrl+U` | clear the current editable field |
| `Esc` | close the form |

While a create is in flight the form is frozen: selectors, field edits and
re-submission are blocked until the response.

## Slash commands

Only lines whose first non-whitespace character is `/` are parsed
(`src/command.rs`). Unknown commands and bad arguments show a local notice
and never produce an RPC command.

| Command | Behavior |
|---|---|
| `/new` | open the new-session form |
| `/resume` / `/sessions` | open the session selector |
| `/model` | open the model selector (target: a new session) |
| `/reasoning` | open the reasoning selector (target: a new session) |
| `/theme dark` / `/theme light` | switch the palette |
| `/clear` | wipe only the local transcript view and reload the active session (never deletes the agent session; refused while a turn runs) |
| `/help` | open the Help panel |
| `/logs` | open the agent-log panel |
| `/quit` | normal shutdown |

Not implemented (spec 23.4): `!command`, `@file`, `/fork`, `/branch`,
`/compact`, `/steer`, `/queue`, `/settings`, `/login`, `/plugin`, `/mcp`.

## Honest scope

- Tools run automatically; bash is not sandboxed.
- No approval UI, no steering, no compaction, no live bash output in v0.1.
- A model/reasoning selection creates a new session; the active session is
  never hot-swapped.
- Phase 5 does not spawn the agent yet, so `session.create`/`session.open`/
  `session.transcript` requests are gated by the `Starting` connection and
  surface as a local notice.