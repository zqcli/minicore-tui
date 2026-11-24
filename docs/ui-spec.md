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
3. **One row per fact until asked.** A step of work is one row. Detail opens in
   place (click or Ctrl+O) and closes the same way.
4. **Show what is true now.** Status, counts and hints come from native Host
   state. A hint appears only when its key does something at that moment.
5. **One vocabulary.** The same thing has the same name, glyph and key
   everywhere (see [Wording](#wording)).
6. **Stable reading position.** Rendering, folding and history loading never
   move what the user is reading. Folding opens downward.

## Screen layers

From top to bottom:

| Layer | Rows | Content |
|---|---|---|
| Transcript | rest | Welcome card in front of block 0, then blocks; scrollbar in the last column; the jump bar when scrolled up |
| Dock | 1 + special rows + 1 | A blank gap row; approval, question or compaction rows when present; the activity line |
| Editor | 1–12 | Rail `▎` plus the draft on the composer surface; grows with content, capped at about a third of the height |
| Footer | 1 | Where you are (left) and what runs (right) |

Pickers replace the dock and editor as a bottom sheet. Full-screen views
(Help, Diff, Error details, Context, Session) replace the transcript and dim
the editor, whose placeholder says `view open · esc back`.

### Gutter and columns

- Column 0 is a blank gutter and ignores clicks.
- The transcript's last column belongs to the scrollbar. Right-aligned facts end
  one column before it.
- Body text starts at the inset (2 cells); step rows start with one space and a
  glyph, so the glyph sits in the column of a rail.

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

One fixed row directly above the editor. It never grows.

- **Left, what is happening:** `⠋ Working 3m · running bash · request 4 ·
  2 tool rounds`, or `Waiting for approval`, `Interrupting…`, a compaction
  phase. Otherwise the newest flash, otherwise the latest notice (errors add
  ` · /error`). Idle: empty.
- **Right, keys that work now**, separated by two spaces, in this order:
  `↰ esc back`; one of `paused · enter resumes`,
  `enter steer · alt+enter follow-up` or `esc interrupt`;
  `N queued · alt+↑ edit`; `N delegated · ctrl+b`.
- **Narrow terminals:** the left keeps at least 28 columns (or its own width).
  Key hints are dropped from the front, so the mode hint goes before the counts.

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

- Empty: one row, rail plus the dim placeholder
  `Ask anything · / commands · @ files`.
- A closed Session says `session closed · /new starts a new one`.
- No padding rows; the caret starts right after the rail.
- Large pastes become `[paste #N +K lines]` chips on the paste surface.

## Transcript blocks

Every block has one of a few shapes. Keep new content in these shapes.

| Block | Shape |
|---|---|
| User message | Accent rail `▎` on the composer surface; its own line breaks; timestamp and `↗ steering` right-aligned on the first row; more than 12 lines fold to `… N more lines` |
| Assistant answer | Markdown at the inset, no rail |
| Thinking (folded) | ` ∴ newest heading  +N` in dim italic, no rail, aligned with tool rows |
| Thinking (open) | Neutral rail, dim italic, bold headings, original line breaks |
| Tool (folded) | One step row; see below |
| Tool (expanded) | Card: neutral rail on the card surface (failure rail and surface when failed), title row, subject, body, footer facts |
| Agent card | Card titled by the task; Agent, short Session ID, rounds and current tool; preview rows; footer opens the child |
| Delegated result | Card with outcome and final answer |
| Turn end | `   completed in 31s · 5 requests · 4 tool rounds · 1 file changed (+39 −0) · /diff` in meta colour |
| Failed Turn | `   failed · 47s · 2 tool rounds` and the message behind a quote rail `│`, three rows until expanded |
| Compaction | Notice rail with its native source position and covered items |

### Step rows

A folded tool is exactly one row:

```
 ✓ read   test/server.test.js                                   45 lines
 ● bash   node --test                                            running
   │ ok 18 - parses empty arrays
   │ ok 19 - rejects negative length
 ✗ edit   src/server.js
   └ edit requires exactly one match, or replace_all for multiple matches
 ⊘ write  notes.txt
```

- Glyph: `●` pending or running (accent), `✓` succeeded with exit code 0
  (success), `✗` failed, result error or non-zero exit (failure), `⊘` denied or
  cancelled (meta). The glyph is chrome and is not copied.
- Name: the real tool name, padded to 6 cells, in the tool-title colour.
- Subject: the path, the first command line (without `$ `) or the pattern; one
  line, cut with `…`.
- Facts, right-aligned in meta: duration, `N lines`, `+A −R`, `exit N`; a
  running tool with nothing else says `running`.
- A running tool shows its last 3 output lines under `   │ `.
- A failure shows its reason once under `   └ `.

Adjacent step rows (tools and folded thinking) stack with no blank row. Every
other pair of visible blocks has exactly one blank separator row.

### Turn groups

When a Turn ends with at least two successful tools, its steps fold under one
row placed before the Turn's first step row, even when that row is a failure:

```
 ▸ Worked 42s · 14 tools · 2 files changed (+3 −1) · 6 failed
 ✗ apply_patch src/server.js
   └ patch contents or positions do not match the existing file
```

- Steps are successful tools without an Agent card or delegation, and
  thinking-only responses. Failed, denied and Agent tools never fold; answers
  never fold.
- `▾` when open. Click toggles; Ctrl+O opens every group along with everything
  else.
- Live Turns are never grouped. Grouping is applied when the Turn ends and
  again when it is reopened from history.

## Colour tokens

Defined in `ui/theme.rs` for dark and light; both palettes define every token.
Only the listed roles may use a token.

| Token | Role |
|---|---|
| `text` | Body text |
| `dim` | Secondary text, thinking, info notices |
| `meta` | Facts, timestamps, Turn ends, `⊘` |
| `faint` | Key hints, counts like `+N` |
| `accent` | User rail, `●`, spinner, pointer and selected item |
| `success` | `✓`, added lines |
| `failure` | `✗`, failure reasons, error notices, removed lines, failure rail |
| `warning` | Usage notices, risk labels |
| `border` | Sheet rules, the welcome card |
| `composer_bg` | Editor and user-message surface |
| `card_bg` | Expanded card surface (success uses the same surface) |
| `card_failure_bg` | Failed expanded card surface |
| `thinking_rail`, `notice_rail`, `status_rail`, `card_rail` | One neutral grey |
| `heading`, `inline_code` | Markdown headings and inline code (accent family) |
| `code_block` | Code block text (muted) |
| `selection_bg`, `selection_fg` | Mouse selection and the selected sheet row |

Rules:

- No new rail colours. A new state is a glyph.
- `NO_COLOR` must leave every state readable through glyphs and words.
- The theme follows `COLORFGBG` when `tui.json` does not set one.

## Glyphs

| Glyph | Meaning | Where |
|---|---|---|
| `▎` | Rail | User messages, editor, expanded cards, open thinking |
| `●` `✓` `✗` `⊘` | Running · done · failed · denied or cancelled | Step rows, delegations sheet |
| `∴` | Thinking | Folded thinking row |
| `▸` `▾` | Folded · open group; footer identity | Turn groups, footer start |
| `│` | Output or quote | Live tool output, failure message |
| `└` | Reason | Under a failed step |
| `→` | Pointer | Selected sheet row |
| `✓` (pointer column) | Current item | Model, reasoning and Session lists |
| `↗` | Steering | User message meta |
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
  detail right-aligned or in an aligned column. The selected row uses the
  selection surface.
- Notes (a preview of the selected row) under the list, after one blank row.
- Keys in the bottom rule, verb after key: `enter open`, `ctrl+n rename`,
  `ctrl+d delete`, `esc close` (or `esc cancel`, `esc back`).
- Long lists window around the selection with a `(k/n)` counter.
- Empty lists say what is missing and how to get it, in one row.

### Full-screen views

Help, Diff, Error details, Context and Session. They follow the same rules,
scroll with arrows, PageUp/PageDown and the wheel, and `Esc` returns with the
transcript position intact. `c` copies the view's content when it has one.
Help filters as you type.

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
- **Click** toggles what it hits: a step row, card, group row or failure.
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
| delegation, child Session | subtask, sub-agent session |
| `esc close`, `esc cancel`, `esc back` | `esc to return`, `press esc` |
| `1 tool`, `2 tools`, `N lines`, `N files changed` | `N tool(s)` |
| completed, failed, cancelled | done, error, aborted |

- Lowercase key names in hints (`ctrl+b`, `alt+↑`); capitalised in prose
  documentation (`Ctrl+B`).
- Separators: ` · ` between facts, two spaces between key hints.
- No trailing periods in rows, hints and notices that are fragments.

## Checklist for a visible change

1. Does it fit one of the block shapes, the sheet template or a view?
2. Does it add colour? Use a glyph instead, unless it marks the user or a
   failure.
3. Is every hint actionable now, and is it in the activity line or a sheet rule?
4. Does it read correctly at 60 columns, with CJK text and with `NO_COLOR`?
5. Is the reading position preserved when it appears, changes or folds?
6. Are selection and copy limited to content (glyphs, rails and hints are
   chrome)?
7. Update snapshots, F1 help, [keybindings.md](keybindings.md), README and the
   changelog; update this document if a rule changed.
