# Keys and Commands

The keymap is compiled into the binary. `F1` opens the same key and command
reference inside the terminal. An open picker or view owns its keys; otherwise
a pending interaction owns them, then the conversation composer. `F8` opens
local input confirmations even from the fatal screen. A key press clears the
transient notices, while native blocking and observation errors remain visible.
`/error` retains the last operation error in full for this TUI process, including
its original Session identity when known. Informational notices and navigation
do not replace it; a later operation error does. It is not a separate history log.
Asynchronous compaction failure notices preserve the reading position and name
the originating Session when it is in the background. Repeated snapshots and
navigation do not replay the same failure; successful, unchanged and cancelled
compactions do not produce failure notices.

`Shift+Enter` and `Ctrl+Enter` depend on the terminal reporting their modifiers.
`Ctrl+J` inserts a newline, and `Alt+Enter` and `Ctrl+Enter` request a follow-up.
If the terminal reserves one of these combinations, configure it to pass the
key through or use the other combination.

### Option+Return on macOS

Use the terminal profile's Option-as-Alt/Meta setting so the modifier reaches
the TUI. This restores the Agent-era frontend's terminal setup guidance:

| Terminal | Setting |
|---|---|
| Terminal.app | Settings → Profiles → select the active profile → Keyboard → **Use Option as Meta key**. |
| iTerm2 | Settings → Profiles → Keys → General → set the left or right Option key you use to **+Esc**. |
| Ghostty | Set `macos-option-as-alt = true`, or `left` / `right` for one Option key. |
| kitty | Set `macos_option_as_alt both`, or `left` / `right`, then open a new kitty process. |

These settings affect other Option shortcuts too. For an iTerm2 mapping limited
to this key, add Option+Return in the active profile's Keys → Key Mappings,
choose **Send Hex Code**, and enter `0x1b 0x0d` (ESC CR).

The composer accepts legacy ESC CR and reported Alt/Ctrl+Enter. On Unix, it also
accepts ESC LF, which crossterm reports as Ctrl+Alt+J. Plain LF/Ctrl+J still
inserts a newline, and plain Enter still steers. A terminal that sends only the
same CR or LF as an unmodified key has discarded the modifier; the TUI cannot
distinguish it. Check the actual queue label (`↳ next turn`) while a Turn runs.
Windows key handling and paste behavior retain their existing meanings.

Terminal references: [Apple](https://support.apple.com/guide/terminal/trmlkbrd/mac),
[iTerm2](https://iterm2.com/documentation-preferences-profiles-keys.html),
[Ghostty](https://ghostty.org/docs/config/reference#macos-option-as-alt),
and [kitty](https://sw.kovidgoyal.net/kitty/conf/#opt-kitty.macos_option_as_alt).

## Agents, Sessions and input queues

`/agents` selects an actual Host Agent. Each Session belongs to one Agent; the
sessions picker shows that owner and uses the full Agent and Session identity
for opening, renaming and deleting. Every Session row includes its stable
six-character ID prefix, even when long titles clip to the same text; renaming
or changing running state does not change that ID. The Agent picker includes lifecycle,
storage mode, Session count and default Session. Selecting an Agent opens its
default Session when one is configured; otherwise it leaves a new-conversation
draft. `/new` creates a Session in the selected Agent. Sending the first prompt
from a draft also creates a Session and sends that prompt after creation.

Agent controls follow the Host's actual progress. A stop remains `stopping`
until its owned work settles, and deletion follows the native lifecycle
until the Agent is removed. The picker shows metadata saves and native save
or deletion faults. Changing the default Session stays bound to its original
Agent even if the terminal switches to another Agent while the save runs.

Switching Sessions does not interrupt their work. A resident Session keeps its
draft and transcript scroll position. The Session catalog also shows Sessions
that are not open in this terminal, including their native lifecycle,
residency, scheduling and pending-input status. The footer shows where you
are on the left (workspace, branch, Session title, and background work as
`N running` and `N needs input`) and what runs on the right (model,
reasoning and approval mode). Open a Session to answer its interaction.

The row above the editor is the activity line. Its left side says what is
happening now: the working spinner with whole minutes, the phase or running
tool, retries and request counts; otherwise a fresh flash or the latest
notice. Its right side lists only the keys that apply: `↰ esc back` in a
child, `paused · enter resumes`, `enter steer · alt+enter follow-up` while
drafting during a Turn, `esc interrupt`, `N queued · alt+↑ edit` and
`N delegated · ctrl+b`. On a narrow terminal the key hints give way before
the status text, the mode hint first.

`Enter` sends `Steer`; `Alt+Enter` or `Ctrl+Enter` sends `FollowUp`. These are
requests to Host scheduling, with different intended queues:

| Input | Requested queue | Meaning |
|---|---|---|
| Steer | next-step | Make the input available to the active Turn at a step boundary, or start work when the Session can consume input. |
| Follow-up | next-turn | Wait for a new Turn. |

The Host decides the actual queue. For example, a steer accepted while a Turn
is cancelling or finalizing can go to next-turn. The activity line counts the
actual unbound records as `N queued`; it does not predict delivery from the
key that was pressed. `Alt+↑` brings their text back for editing.

`/pause` pauses input consumption; `/resume` asks the Host to resume. An empty
`Enter` also resumes a paused Session. Sending a nonempty draft submits input;
the displayed scheduling state determines whether consumption is paused.
`/load` loads a Session without resuming paused input. `/unload` and `/close`
are separate native operations. Host lifecycle or resource failures are shown
as errors, without an automatic stop-and-retry sequence.

Esc cancels the captured Turn and preserves accepted queued inputs. Cancellation
does not imply that scheduling is paused: subsequent input follows the Host's
actual scheduling state. Use `/pause` before cancelling when queued work must
stay pending, then `/resume` when it should continue.

`Alt+↑` recalls queued inputs into an empty draft. Only records whose native
removal succeeds are returned; an input already consumed by a Turn cannot be
recalled. The returned text is separated by blank lines. Clearing a local
confirmation in `F8` does not remove an accepted input from the Host queue.

## Composer

| Key | Action |
|---|---|
| `Enter` | Send a steer; with an empty draft, resume a paused Session. A recognized slash command runs instead of sending text. |
| `Alt+Enter`, `Ctrl+Enter` | Send the draft as a follow-up; an empty draft does nothing. |
| `Shift+Enter`, `Ctrl+J` | Insert a newline. |
| `Esc` | Close a picker or view first. In the composer, clear a selection; with an empty draft in a child Session opened from its card or `/child`, return to the parent; otherwise interrupt the active Turn while preserving queued inputs, cancel manual compaction, or clear an idle draft (Ctrl+Z restores it). |
| `Ctrl+C` | Copy an editor selection; otherwise clear the draft. A second press within 500 ms quits. |
| `Ctrl+D` | Quit with an empty draft; otherwise delete after the cursor. |
| `↑`, `↓` | Move within a multiline draft, then browse prompt history at its first or last line. |
| `Alt+↑` | Recall still-queued inputs into an empty draft. |
| `Ctrl+X` | Cut an editor selection; otherwise copy the last assistant answer. |
| `Ctrl+G` | Open the external editor configured by `ui.editor` in `tui.json`, then `$VISUAL`, then `$EDITOR`. Failures preserve the original draft; if terminal restoration fails, the error names the retained edited file. |
| `/` at draft start | Open the command palette. |
| `@` at word start | Open the Host workspace file picker; an `@` within a word stays text. |
| `Home`, `End`, `Ctrl+A`, `Ctrl+E` | Move to the line start or end. `Home` with an empty draft and `End` also navigate the transcript as described below. |
| `←`, `→` | Move by grapheme. |
| `Ctrl+←`, `Ctrl+→`, `Alt+←`, `Alt+→` | Move by word. |
| `Backspace`, `Delete` | Delete a selection or the adjacent grapheme. |
| `Ctrl+W`, `Ctrl+Backspace`, `Ctrl+H`, `Alt+Backspace` | Delete the previous word. |
| `Alt+D`, `Alt+Delete` | Delete the next word. |
| `Ctrl+U`, `Ctrl+K` | Delete to the line start or end. |
| `Ctrl+Z`, `Ctrl+Y` | Undo or redo. |
| Paste | Insert one edit. Newlines stay draft text; a large paste becomes a chip that expands on send. |

Hold `Shift` with arrows, Home or End to extend an editor selection;
Ctrl/Alt+Shift+Left/Right extends by word. Typing, paste or newline replaces the
selection. Plain Left/Right collapses it to the corresponding edge. Graphemes
and paste chips remain whole during movement and deletion. Recalling a prompt
restores its paste chips and preserves the draft that Down returns to.

Each actual forward Delete or nonempty Ctrl+D is a separate undo step and
clears an abandoned redo branch. Deleting at the end with nothing to remove
preserves valid redo. Undo and redo restore the complete text, cursor,
selection and paste chips together.

Command arguments are checked against the actual catalog before anything is sent: an unknown model or a reasoning level the model does not support shows the valid choices as a usage note and keeps the draft. The palette lists commands by use (session, context, model, session control, Agents, terminal) and shows the current model, reasoning, approval and theme. A bare unknown `/word` produces a suggestion before it is sent. Press `Enter`
again to send it as text, or start with `//` to escape the initial slash.
Paths such as `/usr/bin` and ordinary sentences remain prompts. Prompt
templates appear alongside commands and expand before submission.

## Transcript and mouse

| Key or gesture | Action |
|---|---|
| `PageUp`, `PageDown` | Scroll by a viewport minus four rows. |
| `Ctrl+Home`, or `Home` with an empty draft | Jump to the top of the loaded history and request an older page. |
| `End` | Move to the line end and follow new output again. |
| `Ctrl+↑`, `Ctrl+↓` | Jump between user messages. |
| `Ctrl+O` | Expand or fold tool cards, thinking, summaries and failure details. |
| `Ctrl+T` | Hide or show thinking. |
| `Ctrl+F` | Find within the loaded transcript. |
| `Ctrl+R` | Open the Sessions picker. |
| `Ctrl+L` | Open the model picker. |
| `Ctrl+P`, `Shift+Ctrl+P` | Select the next or previous model when those modifiers are reported. |
| `Shift+Tab` | Cycle the selected model's supported reasoning preferences, including auto. |
| `F1` | Open help; type to filter its rows. |
| `Ctrl+B` | Open the delegations of this conversation; Enter opens the selected child Session. Same as `/children`. |
| `F8` | Review local input confirmations. |
| `q`, `Esc` on the fatal screen | Quit. |
| Wheel | Scroll one row, or five with Alt. Over a picker, move its selection; over a view, scroll that view. |
| Click a tool or Agent row, or a card | Toggle its folded state. Dragging selects instead. |
| Click a `∴` thinking row | Open or fold that thinking. |
| Click a `▸ Worked …` row | Open or fold the finished Turn's steps. |
| Click an opened Agent card's footer | Open its child Session, or explain that its history is unavailable. |
| Click a later send's locate footer | Jump to its original Agent card. |
| Click a failed Turn's message or end line | Read its complete error, including its original Session and Turn identities. |
| Click a Turn-end line with changes and no error | Open the live workspace Git diff. |
| Click or drag in the editor | Place the caret or select draft text, respecting grapheme and paste-chip boundaries. |
| Shift+click in the editor | Extend its selection, when the terminal passes the gesture through. |
| Drag, double-click or triple-click in the transcript | Select text, a word or a line; release copies it. |

Scrolling upward anchors the content being read. New output does not move that
anchor. A `Jump to latest message · End` row appears above the bottom; click it
or press End to follow again. Folding opens and closes downward from the
anchored view.

Separate Responses thinking parts retain their line breaks while streaming and
after reopening a Session. Folded tools are one row each: a state glyph
(`●` running, `✓` done, `✗` failed, `⊘` denied or cancelled), the tool
name, its subject and facts such as duration, line count or `+N −M` at the
right. A running tool shows its last three output lines under the row; a
failure says `failed`, and its reason is in the opened card. Folded thinking
is one `∴` row with its newest heading. Adjacent step rows stack without blank
rows; other blocks keep one separator row. Opening a row keeps its left edge,
and an opened card keeps blank lines inside its output.

When a Turn finishes with at least two successful tools, its steps fold under
one summary row: `▸ Worked 42s · 14 tools · 2 files changed · 1 failed`.
Failed, denied and Agent tools never fold into the group, and the answer text
stays visible. Click the row to open the steps; `Ctrl+O` opens every group.

A delegation is one Agent card on the call that started it, and it keeps
updating after that call returns. Folded, it is one row: the task's first line,
the Agent, tool rounds and state, with the child's three newest steps under it
while it runs. Opened, it shows the task, the short Session ID and later sends,
the child's tool calls, and its result: the delivered final answer, or while it
runs the latest response. Later sends into the same child and the result it
delivers are drawn inside the card rather than as their own blocks; they keep
their own blocks only when the starting call is not loaded. Only Host's
settled delegation marks completion. Click the opened card's footer or use
`/child` to read the child Session and answer its approvals there; Esc
returns to the parent. Child events do not redirect the parent's input or change its draft.
If a temporary child has been released, the card keeps its observed preview
and identifies the unavailable full history.

Only content is copied from the transcript. Rails, padding, timestamps,
durations, expansion hints, partial-response labels and Turn-end summaries are
excluded. Expanded cards keep a title row and include the invocation arguments of tools whose input is not already
their subject or body (bash commands, written content and diffs are), retained
output and any native child Agent/Session association. Long cards retain a
bounded visible preview; `/export` reads the complete native history. Partial
model responses are marked explicitly, and compaction markers report actual
source position and covered items.

Failure messages initially show up to three wrapped rows. `Ctrl+O` expands them
in place. The detail view and `c` copy contain the complete terminal-safe message;
the native history export also retains Host's message independently of folding.
Truncated tool-result previews name `/export` as the complete-result path; their
line counts describe the complete result, not just the retained preview.

## Pickers and reports

Pickers use a bottom sheet framed by two rules: the title and count sit in the
top rule and the keys in the bottom rule. `Esc` closes it. In selectable
lists, arrows and Tab move one row and wrap; PageUp/PageDown move ten rows.
The inline command and file lists stay below the editor and show five rows at
a time.

Find and the Sessions, Models and Agents query fields accept terminal paste
and append it to existing text. Matches refresh and selection returns to the
first result, just as when typing. Newlines become spaces, tabs become four
spaces, terminal escape sequences are removed and other controls are shown
with visible escapes. Ordinary Unicode is preserved. Paste does not accept a
row or alter the conversation draft; Esc returns to that draft. Changing a
Session filter clears its pending delete confirmation.

| Surface | Actions and facts |
|---|---|
| Commands | Type to fuzzy-filter. Enter runs the selected command; Tab completes its name. Argument completion is available for model, reasoning, theme and approval. |
| Agents | Filter by name or ID. Enter selects the Agent. Ctrl+D opens a confirmation with its name, ID and captured child Session list; only `y` deletes that set. Enter does not confirm. Sessions added after confirmation opens are not included. |
| Sessions | Rows show the saved title or, if untitled, the short Session ID. Filter by title, Session ID, Agent ID or workspace folder, not message text. Enter opens the selected Session. Ctrl+N renames it; Enter saves and Esc cancels. Ctrl+D opens a confirmation naming that exact Session; only `y` deletes it. |
| Delegations (Ctrl+B, `/children`) | Rows show a state glyph, the task's first line, the short child Session ID, status and tool rounds; the count reads `N running · M total`. Enter opens the child Session; Esc there returns. |
| Session (`/session`) | A full-screen view. Show Agent and Session IDs, title, workspace, storage, lifecycle, residency, scheduling, approval mode, model, reasoning, raw history count and latest response usage. Enter or `c` copies the Session ID. |
| Context (`/context`) | A full-screen view. Show configured and active model facts, native history/projection counts, queue counts, compaction and save state. `r` refreshes; `c` copies. |
| Models | Filter model references; Enter selects one. The declared context window is model metadata. |
| Reasoning | Enter applies the selected native preference. |
| Files (`@`) | Filter the returned workspace paths. Enter or Tab inserts `@path `. Enter with no match does nothing. This inserts text, not an attachment. |
| Find | Type a case-insensitive query. Enter, Down or Tab advances; Shift+Enter or Up goes back. Matching folded content opens. Esc leaves the transcript at the selected match. |

The Context report does not display an invented current token total or
percentage. Provider input/output/cache/reasoning counts belong to the latest
complete response, and absent fields stay unknown. A compaction request-token
estimate is labeled as an estimate; it is not a measurement of current context
occupancy. Response usage appears only in `/context` and `/session`, never
in the footer.

## Approvals and tool questions

The pending native interaction identifies the Turn, load, interaction and tool
call being answered. Approval cards show the Host's risk level and the known
invocation. There is no terminal shortcut that grants permanent approval to a
tool.

| Interaction | Keys |
|---|---|
| Approval | `y` allows this call; `n` or Esc denies it. Arrows select Allow once or Deny; Enter submits that choice. |
| Single-choice question | Arrows select an option; Enter sends its native index. Esc cancels the requesting Turn. `y` and `n` are not answer shortcuts. |
| Text question | Type in a separate answer editor; Shift+Enter or Ctrl+J inserts a newline. Enter sends the answer, and Esc cancels the requesting Turn. |

Answer keys are armed after 250 ms; unrelated typing during the initial
window postpones approval/choice arming. Esc remains available immediately.
The text answer editor preserves whitespace and the conversation draft. Host
validation errors remain visible and leave the answer editable. Once an
answer is submitted, further answers wait for the Host result.

Ctrl+R remains available during every pending interaction, including while an
answer is awaiting confirmation. It opens the Session picker without changing
the interaction's target or its separate answer draft.

PageUp/PageDown, Ctrl+Up/Down, Ctrl+O and Ctrl+T remain available for reading the
transcript while a question waits. Approval and choice sheets also accept End
to return to the latest content. Full-screen views and bottom sheets do not
expose the hidden conversation draft for mouse editing.

## Views

| View | Keys |
|---|---|
| Git diff | Up/Down selects a file. PageUp/PageDown, Home/End and wheel scroll; `c` copies the diff. Native workspace failures and stale-version notices are shown. |
| Help | Type to filter rows by key or action; Backspace edits the filter. Up/Down scrolls three rows; PageUp/PageDown scrolls thirty. |
| Context, Session | Full-screen reports; `c` copies, `r` refreshes Context. |
| Error details | Up/Down scrolls three rows; PageUp/PageDown, Home/End and wheel scroll. `c` copies the complete message, including content outside the viewport. |

Esc returns to the transcript with its scroll position intact. The dimmed
composer says `view open · esc back`; unbound view keys do not edit it.

## Commands

| Command | Effect |
|---|---|
| `/agents` | Select an Agent from the Host catalog. |
| `/agent-new <name>` | Copy the selected Agent's native configuration into a new dynamic Agent. |
| `/agent-start` | Start the selected Agent through its native lifecycle operation. |
| `/agent-stop` | Stop the selected Agent through its native lifecycle operation. |
| `/agent-delete` | Review and explicitly confirm deleting the selected Agent and the captured child Session set. |
| `/new` | Create and open a Session in the selected Agent. |
| `/sessions` | Open the Sessions picker. |
| `/session` | Show the current Session and its owning Agent. |
| `/default-session` | Make the current Session its Agent's default. |
| `/rename [title]` | Rename the current Session and refresh its list and detail metadata, even after switching Sessions. Without a title, the draft becomes `/rename <current title>` to edit. |
| `/children` | List this conversation's delegations, newest first; Enter opens the child. Ctrl+B does the same. |
| `/child [n]` | Open the latest delegated child Session of this conversation, or the nth latest. Esc with an empty draft returns to the parent. |
| `/cancel` | Interrupt the running Turn and keep queued input; in a child opened from its card this is how to stop it. |
| `/pause` | Pause input consumption in the current Session. |
| `/resume` | Resume the current Session. |
| `/load` | Load the current Session without resuming paused input. |
| `/unload` | Request native unload of the current Session. |
| `/close` | Review and confirm closing the current Session; only `y` closes it. A closed Session keeps its readable history and its composer says so. |
| `/model [reference]` | Select an exact model reference or unique fuzzy match; with no argument, open the picker. |
| `/reasoning [level]` | Apply a native reasoning preference; omit the argument to open the picker. Invalid values show the supported levels without changing the current value. |
| `/approval [ask\|auto\|read_only]` | Update the current Session's approval policy for subsequent Turns; without an argument, list the choices with the current one marked. |
| `/compact` | Request manual context compaction; acceptance is not completion. A terminal failure shows its native fault once in a conversation notice; `/context` retains the details. |
| `/context` | Read native context facts and past response usage. |
| `/diff` | Read live Git changes, including untracked files, from the Session's Host workspace. |
| `/copy` | Copy the last assistant answer as Markdown. |
| `/export [path]` | Export native history to Markdown; the default is `./minicore-<session>-<date>.md`. |
| `/theme [dark\|light]` | Choose and save a palette; without an argument, list the choices. F4 toggles. |
| `/reload` | Ask Host to reload its bound JSON directory; refresh actual catalogs and local templates after full or partial application. |
| `/recover` | Refresh current Session state, history and observations without resending inputs. |
| `/unconfirmed` | Review local inputs whose acceptance is uncertain or was rejected. |
| `/error` | Read or copy the last operation error in full after its notice disappears. |
| `/help` | Open help. |
| `/quit` | Quit the terminal. |

Model, reasoning and approval changes are Session configuration updates. An
active Turn keeps the configuration already bound to it. Host validation is
authoritative; the terminal does not silently substitute a different reasoning
level. With no Session open, model and reasoning selections apply when a new
Session is created. Reloading Agent declarations does not rewrite existing Sessions. File-managed
Agents must be changed through their declarations. To remove one permanently,
remove its declaration and explicitly delete that Agent before reloading;
a missing bound declaration makes reload fail instead of releasing ownership.

In `F8` or `/unconfirmed`, `e` or Enter opens a separate editor; `r` reviews a
resend, `c` copies and `d` reviews forgetting the local record. Resend and forget
require `y`; Enter, Esc or `n` cancels that confirmation. A resend is a new
message to the captured original Session and may duplicate work already
accepted. An unavailable original target is not silently replaced with the
current Session, and forgetting a local record does not cancel Host work.
