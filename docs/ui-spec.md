# UI and UX specification

This is the visual and interaction contract for the terminal frontend. New
surfaces follow it; a change that breaks a rule here updates this document in
the same commit. Keys and commands are listed in [keybindings.md](keybindings.md);
this document says how things look and behave, and why.

## Principles

1. **Content first, chrome quiet.** The conversation is the page. Rails,
   borders, hints and counts are secondary and never compete with text.
2. **Two colour signals.** Colour marks where the user spoke (accent) and where
   something went wrong (failure). Everything else is told apart by layout,
   glyphs and weight. Success is a glyph, not a coloured band.
3. **One row per fact until asked.** A step of work is one row, and so is a
   whole delegation. Detail opens in place (click or Ctrl+O) and closes the
   same way; errors wait inside the opened card.
4. **Show what is true now.** Status, counts and hints come from native Host
   state. A hint appears only when its key does something at that moment.
5. **One vocabulary.** The same thing has the same name, glyph and key
   everywhere (see [Wording](#wording)).
6. **Stable reading position.** Rendering, folding and history loading never
   move what the user is reading. Every fold opens and closes in place below
   the row clicked; a collapse may leave blank rows at the end until the view
   scrolls.

## Grid

Two columns carry the whole screen, in the transcript and the dock alike:

| Column (from the block edge) | Holds |
|---|---|
| 0, the rail column | Rails `▎`, step glyphs `✓ ✗ ● ⊘ ∴ ▸`, the spinner, divider starts |
| 2, the content column | Text: answers, tool names, card titles and bodies, activity and notice text |

The terminal's column 0 is a blank gutter that ignores clicks, so block column
0 is screen column 1. The transcript's last column belongs to the scrollbar;
right-aligned facts end one column before it. Opening a step row into its card
keeps both columns: the rail takes the glyph's place and the title the name's.

## Vertical rhythm

1. Exactly one blank row between two visible blocks.
2. Folded tool rows stack with no blank row between them, and each whole row
   is a click target. Folded thinking and Agent rows, and every opened card,
   keep one blank row above and below, also right under a group's summary row,
   so they stand apart and are easy to hit.
3. Surfaces carry their own padding. The user card and the editor have a pad
   row above and below their text; an expanded card has a pad row after its
   facts and separates its sections (activity, result, footer) with one blank
   surface row. Nothing adds a second blank row around a surface.
4. The bottom of the screen is: transcript, one gap row, then, only when there
   is something to say, the activity line, the queued messages and one blank
   row; then the editor and the footer. An idle screen therefore has one blank
   row above the editor, a busy one has a blank row on each side of the status.
   The editor is pinned to the bottom; the transcript gives up the rows.
5. History markers (compaction, imported summary) are one divider row.
6. Markdown keeps one blank row between paragraphs, headings and lists.
   Tables have one rule under the header, and rules between body rows only
   when a row wraps.

## Screen layers

| Layer | Rows | Content |
|---|---|---|
| Transcript | rest | Welcome card in front of block 0, then blocks; scrollbar; the jump bar when scrolled up |
| Dock | 1 + special rows + 0 or 2+ | The gap row; approval, question or warning rows when present; the activity line, queued messages and a blank row when there is activity |
| Editor | 4–12 | Rail `▎` on the composer surface, a pad row, the draft from the second row; grows with content up to about a third of the height |
| Footer | 1 | Where you are (left) and what runs (right) |

Pickers replace the dock and editor as a bottom sheet. Full-screen views
(Help, Diff, Error details, Context, Session) replace the transcript and dim
the editor, whose text row says `view open · esc back`.

### Welcome card

Drawn once, in front of block 0, so it scrolls away with the history:

```
╭─ minicore-tui 0.12.11 ───────────────────────────╮
│ /work/project                                    │
│ f1 keys and commands · / commands · @ files      │
╰──────────────────────────────────────────────────╯
```

It holds identity, the workspace and the three entry points. It does not list
recent Sessions (Ctrl+R does).

### Activity line

One row directly above the editor, present only with content.

- **Left, what is happening:** the spinner in the rail column, then
  `Working 3m · running bash · request 4 · 2 tool rounds`, or
  `Waiting for approval`, `Interrupting…`, a compaction phase. Otherwise the
  newest flash, otherwise the latest notice (errors add ` · /error`), in the
  content column.
- **Right, keys that work now**, separated by two spaces, in this order:
  `↰ esc back`; one of `paused · enter resumes`,
  `enter steer · alt+enter follow-up` or `esc interrupt`;
  `N queued · alt+↑ edit`; `N delegated · ctrl+b`.
- **Narrow terminals:** the left keeps at least 28 columns (or its own width).
  Key hints are dropped from the front, so the mode hint goes before the counts.
- **Queued messages** follow under it in the content column, oldest first:
  `↗` a steer for the next step, `↳` a follow-up for the next Turn, three at
  most and then `… N more queued`. `alt+↑` brings them back to edit.

Elapsed time is whole minutes (`0m`, `42m`, `1h5m`). The line redraws with the
spinner; there is no per-second timer and no timer when idle.

### Footer

```
 ▸ project@main · Fix the parser · 1 running        gpt 6 luna · medium · approval auto
```

- Left: workspace folder, `@branch` when known, the Session title, then
  background work (`N running`, `N needs input`).
- Right: model, reasoning, `approval <mode>`.
- No telemetry: token usage lives in `/context` and `/session`. No elapsed time.
- On a narrow terminal the Session title is cut first, then the workspace;
  the right side stays whole.

### Editor

- At least four rows: a pad row, the draft starting on the second row, and pad
  rows below. It grows with the draft up to its cap and scrolls beyond it.
- An empty Live editor shows no hint; the welcome card and F1 carry them.
- A view says `view open · esc back`; a closed Session says
  `session closed · /new starts a new one`.
- Large pastes become `[paste #N +K lines]` chips on the paste surface.

## Transcript blocks

| Block | Shape |
|---|---|
| User message | Accent rail on the composer surface: pad row, the text with its own line breaks, a meta row with the time and `↗ steering`, pad row. More than 12 lines fold to `… N more lines` |
| Assistant answer | Markdown in the content column, no rail |
| Thinking (folded) | `∴ newest heading  +N` in dim italic; click or Ctrl+O opens it |
| Thinking (open) | Neutral rail, dim italic, bold headings, original line breaks; click folds it |
| Tool (folded) | One step row; see below |
| Tool (expanded) | Card: neutral rail on the card surface (failure rail and surface when failed), title row, subject, body with the failure reason, footer facts |
| Agent | One card for the whole delegation; see below |
| Turn end | `completed in 31s · 5 requests · 4 tool rounds · 1 file changed (+39 −0) · /diff` in meta colour |
| Failed Turn | `failed · 47s · 2 tool rounds` and the message behind a quote rail `│`, three rows until expanded |
| Compaction, summary | One divider row: `─ Context compacted · 79 items · through position 105 · ctrl+o ───`; opened, the summary follows behind a neutral rail |

### Step rows

```
✓ read   test/server.test.js                                    45 lines
● bash   node --test                                             running
  │ ok 18 - parses empty arrays
  │ ok 19 - rejects negative length
✗ edit   src/server.js                                            failed
⊘ write  notes.txt
```

- Glyph in the rail column: `●` pending or running (accent), `✓` succeeded
  with exit code 0 (success), `✗` failed, result error or non-zero exit
  (failure), `⊘` denied or cancelled (meta). The glyph is chrome.
- Name in the content column: the real tool name, padded to 6 cells.
- Subject: the path, the first command line (without `$ `) or the pattern; one
  line, cut with `…`.
- Facts, right-aligned in meta: duration, `N lines`, `+A −R`, `exit N`;
  `running` or `failed` when there is nothing else to say.
- A running tool shows its last 3 output lines under `  │ `.
- A failure's reason is not shown folded; opening the row shows it in the card.

### Work groups

A Turn's work folds while it runs and after it ends. Each run of work between
two user messages is one group, so a Turn steered twice has three groups, each
followed by what the model said to close that run. A child's delivered result
is not a user message and never splits a run:

```
▎ go
▸ Worked 12s · 5 tools · 1 failed
  │ ✓ read src/store.js
▎ also check the tests          (a steer)
▸ Working · 3 tools
  │ ● bash node --test
```

- Closed, a group is two rows: the summary and the newest step. The step row
  changes as the work moves on, so it reads like a list scrolling up.
- While a run goes on, everything in it folds: tool calls (failed ones too,
  counted as `N failed` in the failure colour), Agent cards (counted as
  `N agents`), thinking and text. The newest-step row shows the newest of
  them, a line of streaming text included.
- Once the run is done, text after its last call is its answer and comes out
  of the group; earlier text stays folded as notes.
- A run folds once it makes a call: a plain answer streams in view. Done, a
  run with a single step, or without a call, keeps its own rows.
- Open (`▾`), the steps follow one level in (two columns) under the summary,
  and each opens into its card at that level, so an Agent card or tool card
  reads as part of its group. Click the summary or the newest-step row to
  toggle; Ctrl+O opens every group along with everything else.
- Thinking that is still going shows the spinner in place of `∴`, in the
  newest-step row and in an open group; it settles back to `∴`.
- The summary says `Working` while the run goes on and `Worked` with its
  duration after: the Turn's time for a single run, otherwise from the message
  that opened the run to the one that closed it.

### Agent card

A delegation's whole life is one card on the call that started it. Later
sends into the same child and the result the child delivers are drawn inside
it, not as their own blocks. Only when the starting call is not loaded do they
fall back to their own receipt or result card.

Folded, it is a step row; while it runs, the child's three newest steps follow:

```
● agent  Read every file under src and test and count functions   Reader · 4 tool rounds · working
  │ ✓ read test/http.test.js
  │ ✓ read test/server.test.js
✓ agent  Summarize src/store.js                                    Reader · 1 tool round · completed
```

Opened, it is a card:

```
▎ Read every file under src and test and count functions      ✓ completed
▎ Reader · Session nxbv1q · 4 tool rounds · 1 later send
▎
▎ Activity
▎ ✓ read src/store.js
▎ … 
▎
▎ Result
▎ <the child's final answer as Markdown, 16 rows, then … N more lines>
▎
▎ Open the child Session · click or /child
```

- Glyph: `●` working (accent; warning when the child waits for approval or
  input), `✓` completed, `✗` failed, `⊘` cancelled, `○` no longer observable.
- Inside a work group the card is a step: it folds with the run and opens one
  level in under the group's summary.
- The title is the task's first line; further task lines follow dimmed.
- Activity is a window of the child's three newest entries (tool calls, notes
  `›`, thinking `∴`) under `… N earlier steps`; it moves up as the child works
  and never grows. Settled, the final answer is the Result, not activity.
- The parent's later dealings with the child are part of the card, not rows of
  their own: sends, `inspect`, `update` and `control` calls aimed at it and
  the parent's `wait` calls in the same Turn. The facts line counts them:
  `2 later sends · 1 inspection · parent waited 27s`. A failed call keeps its
  own row, so its error stays visible.
- Result is the delivered answer, or while running the latest response (or
  thinking, with Ctrl+T on); a failure leads with its reason.
- Only the opened card's footer leads into the child Session. Ctrl+B lists all
  delegations of the conversation.

## Colour tokens

Defined in `ui/theme.rs` for dark and light; both palettes define every token.

| Token | Role |
|---|---|
| `text` | Body text |
| `dim` | Secondary text, thinking, info notices, card section labels |
| `meta` | Facts, timestamps, Turn ends, `⊘`, divider labels |
| `faint` | Key hints, counts like `+N`, divider rules |
| `accent` | User rail, `●`, spinner, pointer and selected item |
| `success` | `✓`, added lines |
| `failure` | `✗`, failure facts and reasons, error notices, removed lines, failure rail |
| `warning` | Usage notices, risk labels, an Agent waiting for input |
| `border` | Sheet rules, the welcome card |
| `composer_bg` | Editor and user-message surface |
| `card_bg` | Expanded card surface (success uses the same surface) |
| `card_failure_bg` | Failed expanded card surface |
| `thinking_rail`, `notice_rail`, `status_rail`, `card_rail` | One neutral grey |
| `heading`, `inline_code` | Markdown headings and inline code (accent family) |
| `code_block` | Code block text (muted) |
| `selection_bg`, `selection_fg` | Mouse selection and the selected sheet row |

- No new rail colours. A new state is a glyph.
- `NO_COLOR` must leave every state readable through glyphs and words.
- The theme follows `COLORFGBG` when `tui.json` does not set one.

## Glyphs

| Glyph | Meaning | Where |
|---|---|---|
| `▎` | Rail | User messages, editor, expanded cards, open thinking and summaries |
| `●` `✓` `✗` `⊘` | Running · done · failed · denied or cancelled | Step rows, Agent cards, delegations sheet |
| `○` | No longer observable | Agent cards |
| `∴` | Thinking | Folded thinking row, a group's newest step; the spinner while it goes on |
| `›` | Note | A group's or an Agent card's newest step when it is text |
| `▸` `▾` | Folded · open group; footer identity | Turn groups, footer start |
| `│` | Live output or quote | Under a running step, failure message |
| `─` | Divider | Compaction and summary rows, sheet rules |
| `→` | Pointer | Selected sheet row |
| `✓` (pointer column) | Current item | Model, reasoning and Session lists |
| `↗` | Steering | User message meta row |
| `↰` | Back to parent | Activity line in a child |
| `…` | Cut text or more lines | Anywhere text is truncated |
| `⠋` … | Spinner | Activity line only |

## Sheets and views

### Bottom sheet template

```
─ Title ─────────────────────────────────────── status ─
 > filter
→ selected row                                   detail
  other row                                      detail

  preview notes
─ enter open · ctrl+d delete · esc close ───────────────
```

- Title in the top rule, its count or state at the right of the rule.
- The filter prompt ` > ` directly under the title when the sheet filters.
- Rows: pointer column (`→` selected, `✓` current, otherwise blank), label,
  detail right-aligned or in an aligned column; a long label is cut before its
  detail. The selected row uses the selection surface.
- Notes (a preview of the selected row) under the list, after one blank row.
- Keys in the bottom rule, key then verb: `enter open`, `ctrl+n rename`,
  `ctrl+d delete`, `esc close` (or `esc cancel`, `esc back`).
- Long lists window around the selection with a `(k/n)` counter.
- Empty lists say what is missing and how to get it, in one row.

### Full-screen views

Help, Diff, Error details, Context and Session start with `← Title`: the arrow
says Esc goes back. They scroll with arrows, PageUp/PageDown and the wheel, and
Esc returns with the transcript position intact. `c` copies the view's content
when it has one. Help filters as you type.

### Confirmations

Destructive or irreversible actions (`/close`, Session or Agent deletion,
resend, forget) open a confirmation sheet that names the exact target. Only `y`
confirms; Enter, `n` and Esc cancel.

## Interaction rules

- **Esc** closes the innermost thing: view or sheet, then selection, then back
  to the parent from a child, then interrupts the Turn (queue kept), then clears
  an idle draft (Ctrl+Z restores).
- **Enter** in the editor steers (or resumes a paused Session when empty);
  **Alt+Enter** sends a follow-up. A slash command runs instead of being sent.
- **Click** toggles what it hits: a step row or card, a thinking row, a group
  row, a failure. It folds in place: the row clicked stays where it is.
  Dragging always selects instead of toggling.
- **Ctrl+O** is the one global expand/fold; Ctrl+T hides or shows thinking.
- Commands with fixed choices open their choices when run without an argument;
  invalid arguments keep the draft and say what is valid.
- A notice is one line. Its full text is in `/error` (errors) or the relevant
  view, and the notice says where.
- Hints appear only when the key works. Do not show a hint for a key that would
  do nothing in the current state.

## Wording

| Use | Not |
|---|---|
| steer, follow-up | next-step message, queue message |
| queued | pending, waiting (for queue entries) |
| Session, Agent, Turn | chat, bot, run |
| delegation, child Session, later send | subtask, sub-agent session, receipt |
| `esc close`, `esc cancel`, `esc back` | `esc to return`, `press esc` |
| `1 tool`, `2 tools`, `N lines`, `N files changed` | `N tool(s)` |
| completed, failed, cancelled | done, error, aborted |

- Lowercase key names in hints (`ctrl+b`, `alt+↑`); capitalised in prose
  documentation (`Ctrl+B`).
- Separators: ` · ` between facts, two spaces between key hints.
- No trailing periods in rows, hints and notices that are fragments.

## Checklist for a visible change

1. Does it fit one of the block shapes, the sheet template or a view?
2. Does it sit on the grid: glyphs and rails in column 0, text in column 2,
   and a group's steps one level in?
3. Does it keep the vertical rhythm: one blank row between blocks, none between
   steps, never two blank rows anywhere?
4. Does it add colour? Use a glyph instead, unless it marks the user or a
   failure.
5. Is every hint actionable now, and is it in the activity line or a sheet rule?
6. Does it read correctly at 60 columns, with CJK text and with `NO_COLOR`?
7. Is the reading position preserved when it appears, changes or folds?
8. Are selection and copy limited to content (glyphs, rails and hints are
   chrome)?
9. Update snapshots, F1 help, [keybindings.md](keybindings.md), README and the
   changelog; update this document if a rule changed.
