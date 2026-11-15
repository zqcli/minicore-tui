# Keys and Commands

The keymap is compiled into the binary. `F1` opens the same key and command
reference inside the terminal. An open picker or view owns its keys; otherwise
a pending interaction owns them, then the conversation composer. `F8` opens
local input confirmations even from the fatal screen. A key press clears the
transient notices, while native blocking and observation errors remain visible.
Asynchronous compaction failure notices preserve the reading position and name
the originating Session when it is in the background. Repeated snapshots and
navigation do not replay the same failure; successful, unchanged and cancelled
compactions do not produce failure notices.

`Shift+Enter` and `Ctrl+Enter` depend on the terminal reporting their modifiers.
`Ctrl+J` inserts a newline, and `Alt+Enter` and `Ctrl+Enter` request a follow-up.
If the terminal reserves one of these combinations, configure it to pass the
key through or use the other combination.

## Agents, Sessions and input queues

`/agents` selects an actual Host Agent. Each Session belongs to one Agent; the
sessions picker shows that owner and uses the full Agent and Session identity
for opening, renaming and deleting. The Agent picker includes lifecycle,
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
residency, scheduling and pending-input status. The footer identifies the
selected Agent when there is room and reports background work as `N running`
and `N needs input`. Open a Session to answer its interaction.

`Enter` sends `Steer`; `Alt+Enter` or `Ctrl+Enter` sends `FollowUp`. These are
requests to Host scheduling, with different intended queues:

| Input | Requested queue | Meaning |
|---|---|---|
| Steer | next-step | Make the input available to the active Turn at a step boundary, or start work when the Session can consume input. |
| Follow-up | next-turn | Wait for a new Turn. |

The Host decides the actual queue. For example, a steer accepted while a Turn
is cancelling or finalizing can go to next-turn. The dock displays the actual
unbound records as `↗ next step` and `↳ next turn`, with at most three preview
rows and a count of the remaining records. It does not predict delivery from
the key that was pressed.

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
| `Esc` | Close a picker or view first. In the composer, clear a selection, otherwise interrupt the active Turn while preserving queued inputs, cancel manual compaction, or clear an idle draft. |
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

A bare unknown `/word` produces a suggestion before it is sent. Press `Enter`
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
| `F1` | Open help. |
| `F8` | Review local input confirmations. |
| `q`, `Esc` on the fatal screen | Quit. |
| Wheel | Scroll one row, or five with Alt. Over a picker, move its selection; over a view, scroll that view. |
| Click a tool card | Toggle its folded state. Dragging selects instead. |
| Click a Turn-end line | Open the live workspace Git diff. |
| Click or drag in the editor | Place the caret or select draft text, respecting grapheme and paste-chip boundaries. |
| Shift+click in the editor | Extend its selection, when the terminal passes the gesture through. |
| Drag, double-click or triple-click in the transcript | Select text, a word or a line; release copies it. |

Scrolling upward anchors the content being read. New output does not move that
anchor. A `Jump to latest message · End` row appears above the bottom; click it
or press End to follow again. Folding opens and closes downward from the
anchored view.

Only content is copied from the transcript. Rails, padding, timestamps,
durations, expansion hints, partial-response labels and Turn-end summaries are
excluded. Expanded tool cards include complete invocation arguments, retained
output and any native child Agent/Session association. Long cards retain a
bounded visible preview; `/export` reads the complete native history. Partial
model responses are marked explicitly, and compaction markers report actual
source position and covered items.

## Pickers and reports

Most pickers and reports use a bottom sheet. `Esc` closes it. In selectable
lists, arrows and Tab move one row and wrap; PageUp/PageDown move ten rows.
The inline command and file lists stay below the editor and show five rows at
a time.

| Surface | Actions and facts |
|---|---|
| Commands | Type to fuzzy-filter. Enter runs the selected command; Tab completes its name. Argument completion is available for model, reasoning, theme and approval. |
| Agents | Filter by name or ID. Enter selects the Agent. Ctrl+D opens a confirmation with its name, ID and captured child Session list; only `y` deletes that set. Enter does not confirm. Sessions added after confirmation opens are not included. |
| Sessions | Filter by title, first loaded prompt, Session ID, Agent ID or workspace folder. Enter opens the selected Session. Ctrl+N captures that Session for renaming; Enter saves and Esc cancels. Ctrl+D twice confirms deletion of the same Agent/Session identity. |
| Session (`/session`) | Show Agent and Session IDs, title, workspace, storage, lifecycle, residency, scheduling, approval mode, model, reasoning, raw history count and latest response usage. Enter or `c` copies the Session ID. |
| Context (`/context`) | Show configured and active model facts, native history/projection counts, queue counts, compaction and save state. `r` refreshes; `c` copies. |
| Models | Filter model references; Enter selects one. The declared context window is model metadata. |
| Reasoning | Enter applies the selected native preference. |
| Files (`@`) | Filter the returned workspace paths. Enter or Tab inserts `@path `. Enter with no match does nothing. This inserts text, not an attachment. |
| Find | Type a case-insensitive query. Enter, Down or Tab advances; Shift+Enter or Up goes back. Matching folded content opens. Esc leaves the transcript at the selected match. |

The Context report does not display an invented current token total or
percentage. Provider input/output/cache/reasoning counts belong to the latest
complete response, and absent fields stay unknown. A compaction request-token
estimate is labeled as an estimate; it is not a measurement of current context
occupancy. The footer's `last ↑… ↓… R…` also refers to a past complete response.

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
| Help | Up/Down scrolls three rows; PageUp/PageDown scrolls thirty. |

Esc returns to the transcript with its scroll position intact. The dimmed
composer says `view open · esc to return`; unbound view keys do not edit it.

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
| `/rename <title>` | Rename the current Session and refresh its list and detail metadata, even after switching Sessions. |
| `/pause` | Pause input consumption in the current Session. |
| `/resume` | Resume the current Session. |
| `/load` | Load the current Session without resuming paused input. |
| `/unload` | Request native unload of the current Session. |
| `/close` | Close the current Session. |
| `/model [reference]` | Select an exact model reference or unique fuzzy match; with no argument, open the picker. |
| `/reasoning [level]` | Apply a native reasoning preference; omit the argument to open the picker. Invalid values show the supported levels without changing the current value. |
| `/approval <ask\|auto\|read_only>` | Update the current Session's approval policy for subsequent Turns. |
| `/compact` | Request manual context compaction; acceptance is not completion. A terminal failure shows its native fault once in a conversation notice; `/context` retains the details. |
| `/context` | Read native context facts and past response usage. |
| `/diff` | Read live Git changes, including untracked files, from the Session's Host workspace. |
| `/copy` | Copy the last assistant answer as Markdown. |
| `/export [path]` | Export native history to Markdown; the default is `./minicore-<session>-<date>.md`. |
| `/theme [dark\|light]` | Choose and save a palette; no argument toggles it. |
| `/reload` | Ask Host to reload its bound JSON directory; refresh actual catalogs and local templates after full or partial application. |
| `/recover` | Refresh current Session state, history and observations without resending inputs. |
| `/unconfirmed` | Review local inputs whose acceptance is uncertain or was rejected. |
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
