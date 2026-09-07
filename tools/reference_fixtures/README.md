# Rail Reference Fixture Generator (test-only)

Generates the normalized styled-cell fixtures under
`tests/fixtures/rail/` by invoking the **fixed** pi-rail-ui reference
(`rail_commit` in `PROVENANCE.json`) and its pinned **Pi 0.84.4** renderers.
This is dev tooling for the Rail UI parity work — it is **not** part of any
Cargo build, and default Cargo tests consume only the generated JSON files
already present in the worktree (offline, no Node, no network).

## Provenance pins

- Rail repo: `zqcli/pi-rail-ui` at `1d0dd1611a4d9546c64fe9f5b5c966253fb88eba`
  (subject: *refactor: rename railfast command to rail-oai-fast*), selected by
  `RAIL_REF_DIR` (default: the sibling checkout `../pi-rail-ui-ref-r1`). The
  generator rejects another commit or tracked source modifications.
- Pi packages: every `@earendil-works/pi-*` entry in the reference lockfile must
  be **0.84.4** exactly and installed as a real directory below that checkout.
  The generator imports both Rail and Pi from the selected checkout; a newer
  global Pi or a fixture-directory symlink cannot substitute.
- Theme: Pi's bundled `dark` theme via `initTheme("dark")` — what Rail uses on
  an ordinary startup before a custom theme is chosen. Tracked as `theme` in
  every fixture.
- Determinism: generator forces `TZ=UTC` (en-US timestamps stay stable) and
  overrides `Date.now` where wall-clock invariants (footer duration) are pinned.
  `generated_at` is informational only.

## How to run

```bash
# from repo root; installs both lockfiles and then generates with the selected
# reference checkout:
bash tools/reference_fixtures/bootstrap.sh

# To use an existing installed reference without reinstalling it:
RAIL_REF_DIR=/path/to/pi-rail-ui-ref \
  npm --prefix tools/reference_fixtures run generate
```

`bootstrap.sh` uses `npm ci` only for this test-only generator and the isolated
reference checkout. Ordinary Cargo tests do not invoke Node, access the
reference checkout, or require network access. The generator fails before
writing fixtures when the reference commit, tracked tree, lockfile versions, or
installed Pi package tree does not match the pins.

## What gets invoked (real reference code, not retyped expectations)

| Fixture group | Real functions/renderers invoked |
|---|---|
| `editor` | `RailEditor` (Pi `CustomEditor` subclass) `.setText/.render`, native wrapping, cursor marker, height window, CJK/emoji |
| `paste` | native Pi `Editor.handlePaste` — threshold black-box (marker at 11 lines / 1001 chars, inline otherwise) |
| `slash` | `RailEditor.handleInput("enter")` with autocomplete state — command kept in editor, no submit |
| `surface` | `EditorSurfaceRenderer` geometry + `renderSurfaceRow`, `RailSectionBlock`, all four tool state surfaces |
| `tool` | `renderExecutionRail` + `applyDefaultAutoCollapse`, `executionHiddenLineCount`, `collapseHint`/`simpleCollapseHint`, `collapsedSimpleRows`; real `ToolExecutionComponent` model-tool fixtures are separate from the `bashExecution` reference-only fixtures |
| `user` | `installUserMessageRail` + real `UserMessageComponent`, `UserMessageTimestampRegistry`, `formatUserMessageTimestamp` |
| `thinking` | `renderAssistantMessageRail` -> `AssistantThinkingRailBlock` (real 3-line collapse), `collapseHint` |
| `footer` | `renderSimpleFooter`, `collectFooterLiveState`, footer store (duration/selection notice), `formatNum`, `formatCost`, `FOOTER_LAYOUT` |
| `scrollbar` | `railScrollbarGeometry`, `drawRailScrollbar`, view marking |
| `markdown` | Pi 0.84.4 native `Markdown` + real dark `getMarkdownTheme()` |

## Cell fixture schema (`minicore-rail-cell-v1`)

```json
{
  "schema": "minicore-rail-cell-v1",
  "case": "editor/one-line",
  "term": { "cols": 80, "rows": 24 },
  "cursor": [row, col],           // optional, screen coords of CURSOR_MARKER
  "osczones": [row, ...],          // optional, rows that open an OSC 133 zone
  "rows": [
    [ token, ... ]
  ]
}
```

Each row is a run-length list of tokens:

- a **bare string** = a run of default-style single-column characters;
- an **object** `{"c": chars, "fg": [r,g,b], "bg": [r,g,b], "s": flags, "w": 2}`
  where `fg`/`bg` are 24-bit RGB and omitted means transparent/theme-default
  (terminal default), `s` is a bitmask (1 bold, 2 dim, 4 italic, 8 underline,
  16 strikethrough, 32 reverse), and `w: 2` marks a wide trailing character
  that owns the next screen column (ratatui-style joiner).

Rows with a leading fake-text marker (e.g. `expanded=true`) are plain-text
oracles interleaved before render rows; parse errors or unexpected control
sequences abort generation so fixtures can never silently encode a mismatch.

## Known reference behavior recorded as fixtures

- A cancelled tool stays `isPartial=true` and renders with the **pending**
  surface — the fixed renderer mapping has no distinct cancelled surface.
- `collapseHint`/`simpleCollapseHint` render with the initialized Pi dark theme
  (`initTheme("dark")`): `ctrl+o` in dim #666666, description in muted #808080.