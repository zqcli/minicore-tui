# Rail UI Parity — Stage 0: Source Oracle & Reference Fixtures

Stage 0 pins the three repositories, runs and records baselines, builds a
real source-reference fixture generator, and checks in normalized styled-cell
fixtures from the **fixed** pi-rail-ui + Pi 0.84.4 renderers. No production
renderer/editor/footer code was changed in this stage.

## 1. Repository provenance (pinned during this stage)

| Repo | Checkout HEAD (recorded) | Notes |
|---|---|---|
| `minicore-tui` | `2b8268dbba81c162b30e984b9b31a58ebc3bba65` (`feat: migrate to Agent v0.3 and release TUI 0.2.1`) | work proceeds on dev; no production edits in stage 0 |
| `minicore-agent` | `2d16f554796861a21a49afcd77f4eab74022bf92` (`feat(model): add extended reasoning levels`) | Stage 1 presentation changes are dirty worktree changes; this commit is preserved |
| `minicore-runtime` | `6cd2bdbc634437dea925495c61c7eb0be10ba171` (`feat(model): add extended reasoning levels`) | **not modified**; only validation was run |
| Reference `pi-rail-ui` | **fixed** `1d0dd1611a4d9546c64fe9f5b5c966253fb88eba` (`refactor: rename railfast command to rail-oai-fast`) | isolated read-only checkout at `/Users/zzq/Develops/pi-rail-ui-ref-r1` |
| Pi packages | every `@earendil-works/pi-*` package in the reference lockfile | `npm ci` + generator validation require real installed directories at **0.84.4** |

Notes:
- The live `pi-rail-ui-dev` checkout is HEAD `86c6fe96b2...` (newer, hosted-search
  features), **not** the reference. All fixture generation imports the fixed
  commit checkout instead.
- The generator dynamically resolves every Rail module and Pi package below
  `RAIL_REF_DIR`; it does not use fixture-directory symlinks or a
  newer/global Pi. `bootstrap.sh` installs the reference lockfile and the
  generator's own pinned `tsx` lockfile before running.
- The ReactNode newer Pi is installed as the global user too; it is **not**
  used by the generator.

### Baseline checks (recorded on 2026-09-05, before any edits)

| Check | Command | Result |
|---|---|---|
| TUI build | `cargo build --all-targets --locked` | pass |
| TUI tests | `cargo test --all-targets --locked` | 277 pass, 0 fail |
| TUI fmt | `cargo fmt --all -- --check` | pass |
| TUI clippy | `cargo clippy --all-targets --locked -- -D warnings` | pass |
| Agent tests | `cargo test --all-targets --locked` | 222 pass + aux suites, 0 fail |
| Rail reference | `npm run check` at fixed commit (`node_modules/.bin/tsx`) | 213 tests pass, 0 fail |

After this stage's additions: TUI tests 279 pass (`rail_fixtures` adds 2),
fmt/clippy still clean.

## 2. User reference screenshot

`/Users/zzq/Develops/minicore-tui/reference.png` — present, 3790×1892 PNG (the
spec text mentioned 2048×1067; the actual file is the larger 3790×1892),
placed by the user on 2026-09-05. SHA-256:
`d642dbea3126268675c62d351f189a514d6e7c557e22bdb2709d9b93ba232fe1`.
Not altered or replaced. Used only as a qualitative density/layout target for
acceptance later; pixel colors are not treated as a color oracle (spec §4.3).

## 3. What was delivered

### `tools/reference_fixtures/` — test-only source-reference generator

Invokes the **fixed** reference's real renderers/functions with the pinned
Pi 0.84.4 and writes normalized cell grids. Not part of any Cargo build.

| Module | Real reference code invoked |
|---|---|
| `cases/editor.mts` | `RailEditor` (Pi `CustomEditor` subclass): render, wrapping, cursor, height window, CJK/emoji, wide grids |
| `cases/editor.mts` (paste) | native Pi `Editor.handlePaste` — threshold black-box (marker at 11 lines / 1001 chars) |
| `cases/editor.mts` (slash) | `RailEditor.handleInput("enter")` + autocomplete state: skill command kept, not submitted |
| `cases/surface.mts` | `EditorSurfaceRenderer` geometry + `renderSurfaceRow`, `RailSectionBlock`, 4 tool-state surfaces |
| `cases/tool.mts` | `renderExecutionRail` + `applyDefaultAutoCollapse`, `executionHiddenLineCount`, `collapseHint`/`simpleCollapseHint`, `collapsedSimpleRows` |
| `cases/user.mts` | `installUserMessageRail` + real `UserMessageComponent`, `UserMessageTimestampRegistry`, `formatUserMessageTimestamp` |
| `cases/thinking.mts` | `renderAssistantMessageRail` → real `AssistantThinkingRailBlock` (3-line collapse), `collapseHint` |
| `cases/footer.mts` | `renderFooter`/`renderSimpleFooter`, `collectFooterLiveState`, footer store (duration/selection notice), `formatNum`, `formatCost`, `FOOTER_LAYOUT` |
| `cases/scrollbar.mts` | `railScrollbarGeometry`, `drawRailScrollbar`, view marking |
| `cases/markdown.mts` | Pi 0.84.4 native `Markdown` + real dark `getMarkdownTheme()` |

Determinism: forced `TZ=UTC`; `initTheme("dark")`; Pi's real app keybindings
registered on the **nested** pi-tui copy so `keyHint` renders `ctrl+o`
(`app.tools.expand`, from Pi `core/keybindings.js`); fixed clock via
`Date.now` override for footer duration.

### `tests/fixtures/rail/` — 114 generated fixtures (in this worktree)

| Group | Files | Cases |
|---|---|---|
| editor | 11 | empty, 1-line, 4-line, 13-line cap, cursor mid, long soft-wrap, CJK, CJK+emoji, centered, 120×40, 60×16 |
| editor-click | 3 | native ASCII, wrapped, and wide-grapheme click/cursor cases |
| paste | 7 | 10/11 lines (inline/marker), 1000/1001 chars, single-line, marker+cursor, threshold oracle |
| slash | 3 | popup rows, Enter-keeps-skill-command, Enter-no-list-submits |
| surface | 15 | geometry (3 sizes), surface rows (editor/thinking/user/4 tool states), blank padding, 13→12 window, section blocks |
| tool | 21 | accounting oracle, 4 states (+cancelled-as-pending), reference-only `bashExecution` fixtures, real green `ToolExecutionComponent` simple write/read/bash, real 19/20/21 estimator boundaries, expanded 50 rows |
| user | 5 | timestamp format, registry FIFO, basic/markdown/duplicate-text surfaces |
| thinking | 10 | hint oracle, 2/3/4/10-line collapse+expand, manual-expand-kept, long-wrap, merged-parts |
| footer | 17 | counts/cost oracles, ready/working/queued/copied, duration 0m/59m/1h1m, context 69.99/70/unknown, cost variants, model-short, narrow 40/60/100, layout |
| scrollbar | 4 | geometry top/mid/bottom/overflow/drag-preview, thumb top/mid/bottom cells |
| markdown | 18 | bold/italic/code/link/heading/list/codeblock/quote/hr/wrap/CJK/emoji/strike/nested, padding |

Each JSON documents its source (`rail_commit`, `pi`, `theme`), `term`,
`cursor` (real native marker position), optional `osczones`, `annotations`
(real state notes like `expanded=` / hidden counts / submitted buffers), and
the run-length cell rows (schema `minicore-rail-cell-v1`, see
`tools/reference_fixtures/README.md`).

### `tests/rail_fixtures.rs` — offline integrity gate

Validates every generated fixture present in the worktree (schema, provenance pins, cell width vs
`term.cols`, color bounds, style flags, cursor/osczone bounds, presence of
≥90 fixtures). Runs with no Node, no network, no reference checkout.

## 4. Source-behavior findings recorded by the fixtures

- Pi 0.84.4 native paste markers trigger at `>10 lines` or `>1000 chars`:
  `[paste #N +L lines]` / `[paste #N C chars]`; 10 lines and 1000 chars inline.
- The fixed reference has **no distinct cancelled tool surface**: a cancelled
  (never-completed) tool stays `isPartial=true` and renders pending
  (`state-cancelled-as-pending` fixture). `cancelled` is a resolved style with
  no renderer producer in this version.
- The reference-only `bashExecution` auto-collapse estimate is
  `command(1)+output(n)+1 <= 20` → 18 expanded, 19..68 collapsed; its hint
  uses "`more lines`" (`simpleCollapseHint`). Model `bash` uses the green
  `toolExecution` surface. Real `ToolExecutionComponent` generic 19/20/21
  fixtures record the source estimator's actual args+output hidden counts
  (20/21/22), rather than treating output length as the whole count.
- Thinking collapses at `rawLines.length > 3` (raw logical lines, not soft
  wraps), hidden count `rawLines.length - 3`, preview = first 3 rows; manual
  `setExpanded` persists across streaming updates (`manual-expand-kept`).
- User timestamp `2:05 PM · 9/5/2026` matches the spec example; duplicate
  text gets per-occurrence FIFO timestamps.
- `fitAligned` narrow: right group wins, left is truncated with `…`
  (`narrow-40`); `formatNum` uses 1 decimal k/m over 1000/1M; `formatCost`
  uses 4/3/2 decimals by magnitude.
- Slash: fixed commit keeps `skill:`-prefixed commands in the editor on Enter
  (parity with rail-editor-autocomplete.ts) and sets `autocompleteState=null`.
- The user-specified Rail surface slate remains `#313244`; the pinned native
  Pi Markdown source has a separate `#343541` default in its source theme.
  Native Markdown fixtures therefore remain a content/style oracle, while the
  Rust surface adapter owns the explicit Rail background contract.

## 5. Missing oracle cases (honest gaps for later stages)

Not captured headlessly in stage 0; each is queued for PTY/session-based
capture or a dedicated driver in stages 2–4:

1. On-screen **slash-autocomplete popup cells** (real `SelectList` needs a live TUI focus/render path). Enter contract is locked; popup geometry/styles are not.
2. **Word/segment drag selection, double/triple-click, auto-scroll selection**: selection lives in Pi's TUI focus/input layer, not reachable headlessly; needs the PTY harness.
3. **Clipboard** (`selection copied` footer notice path and copy ranges): needs input + a clipboard; the footer notice text is locked via `selection-copied`.
4. **Scrollbar drag preview→commit animation** (90 ms): `railScrollbarGeometry` honors the pending scrollTop (locked in `drag-preview`); the timers/animation run in the live TUI.
5. **Full mixed Loop cells** (thinking→text→tool→next request in one session): requires assembling real native Assistant/Tool components in a session; components exist headlessly but their cross-section layout needs the transcript container, so this is deferred to the PTY screenshot stage.
6. **User timestamp "time unavailable"** for pre-`user_times` history: the reference shows a fallback `now()`; the "missing history time" display policy is a TUI-side decision to pin during stage 2/6 using the Agent data model.

## 6. Next concrete implementation design (stage 1-first)

1. `test(rail): pin visual reference` — follow-up commits to this stage; first land the fixture gate + rerun rules (`cargo test --test rail_fixtures` offline; `tools/reference_fixtures/generate.mts` with the pinned checkout to regenerate).
2. `feat(agent): bounded presentation data` — Agent `src/presentation.rs` (ToolDisplay, per-session presentation, thin Model/Tool wrappers reusing `AgentEventSink`), History extras (user timestamps via new JSONL metadata, `parts`, optional `display`), one `session.presentation` read-only RPC; no Runtime change. (Independent of TUI UI work.)
3. `refactor(ui): unify rail geometry + one-line dock` — `src/ui/rail.rs` + `screen_layout` replacing `composer_height_phase5`/`footer_height`; remove `Block::bordered`/double footer. Reuse: `editor_height` = `max(4, min(12, floor(rows*0.32)))` from `surface/geometry` fixture.
4. `feat(ui): rail messages + stable per-section folding` — SectionId + PreparedConversation; render User/Thinking/Text/Tool against `tests/fixtures/rail/{user,thinking,tool,markdown}` cells; paste markers + slash + paste thresholds against `paste`/`editor` cells.
5. `feat(editor)`, `feat(ui): selection/copy/scrollbar`, `feat(footer)` — each consuming the corresponding fixture group; footer single line driven by `footer` fixtures.
6. Acceptance: 4 real session screenshots + diff table; honest reporting of gaps (popup cells, selection, context/cost unknowns).

## 7. Initial Stage 0 status

The following was the status when this document was first written. It is kept
as historical staging context; the current implementation matrix is in
[`rail-ui-parity-report.md`](rail-ui-parity-report.md).

- RAIL-33 (reference fixtures committed with pinned provenance): the fixture
  corpus and provenance are present in this uncommitted worktree; the commit
  and CI acceptance gate remain pending.
- RAIL-32 (offline default tests): the local offline fixture gate passes; the
  cross-platform CI and PTY recovery portions remain pending.
- RAIL-01…31: not yet applicable at the initial Stage 0 checkpoint; §8 records
  the follow-up implementation status and the current report records results.

## 8. Follow-up implementation status

The same uncommitted worktree now contains the Agent §12 read-only presentation
extension and the TUI Stage 2–5 display/interaction work. These follow-up changes do not
change `minicore-runtime` or the existing Loop/Steer/History/Blocked/terminal
contracts.

- Agent presentation data is bounded and whitelist-based: tool detail,
  displayable input/result counts, ordered assistant parts, acceptance times,
  Git branch, and explicit unknown context/cost values. Presentation events and
  history views use full `(loop_id, request_index, tool_call_id)` identity;
  sensitive bodies are omitted from `Debug`.
- TUI uses shared Rail surface/layout helpers, stable section identities,
  prepared row/copy metadata, per-section Tool/Thinking fold overrides, the
  one-row footer, fixed editor rail geometry, and same-position mouse toggles.
  RFC3339 acceptance times are displayed in the pinned UTC fixture format, and
  missing old timestamps remain `time unavailable`.
- The offline fixture gate now includes Rust/source comparisons for surface
  geometry, editor height, Thinking fold thresholds, Tool state colors, hidden
  count boundaries, and narrow Footer fitting. The current corpus remains
  uncommitted by request.

The current worktree has since added the remaining local, non-PTY interaction
coverage: native paste-marker projection, slash popup and Enter behavior,
Editor click mapping (including wrapped and wide-grapheme cases), character /
word / paragraph selection, external-spacer-safe copying, clipboard feedback,
selection edge auto-scroll, and release-only scrollbar commit with cancellation
on focus, resize, viewport change, or wheel input. The corpus is now 114 JSON
fixtures and the offline Rail test has nine passing tests. The current E2E
harness is `scripts/stage7_pty.py`; it uses the real binaries and loopback
model, but valid screenshots require a terminal-emulator GUI capture context.

Still outside this follow-up: a complete black-box source fixture for every
selection granularity and locale-specific word segmentation, scrollbar
resize/animation evidence, mixed-loop PTY screenshots and diff tables, and
cross-platform CI. These remain intentionally unclaimed; RAIL-32, RAIL-33,
and RAIL-34 are not closed by local offline results or raw PTY output alone.