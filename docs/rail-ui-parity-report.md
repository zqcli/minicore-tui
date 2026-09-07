# Rail UI Parity Report

Status: **implementation and independent final r5 verification complete;
32 RAIL items PASS within the documented evidence scope, 2 PARTIAL.
Full product-wide parity is not claimed; all changes remain uncommitted.**

The fixed visual source is Rail
`1d0dd1611a4d9546c64fe9f5b5c966253fb88eba` with Pi `0.84.4`.
The final source includes ICU4X dictionary-based CJK selection. Original and
ending repository identities, intentional source differences, and remaining
verification limits are recorded below.

## Evidence

### Independent final verification (r5)

Final TUI logs are archived in `docs/verification/rail/final-r5/`:

- `tui-stable.log` / `tui-msrv.log`: native Linux stable and Rust 1.85 each
  **360 passed / 12 default-ignored**, all targets.
- `tui-fmt.log` / `tui-clippy.log` / `tui-doc.log`: rustfmt, clippy with
  `-D warnings`, and rustdoc with `-D warnings` pass.
- `tui-windows-clippy.log` / `tui-windows-build.log`: Windows GNU all-target
  cross-clippy and test-binary linking (`--no-run`) pass. These are not native
  Windows execution or hosted CI evidence.
- `tui-e2e.log`: **11 real-Agent loopback E2E scenarios pass**, serial, 0.52 s.
- `tui-pty.log` / `tui-pty.typescript`: **1 real-PTY restore test passes**
  under the `script` terminal runner.
- `source-hashes.txt`: final lockfile and key TUI/Agent source hashes match
  the local working tree.

Agent code did not change between r4 and r5. Its independent evidence remains
under `docs/verification/rail/final-r4/`: **263 passed / 2 ignored**, plus
rustfmt, clippy, rustdoc, and **2 RPC-soak tests including the 20-turn scenario**.
The r5 E2E run used the Agent binary built from that same verified source.
The pre-CJK TUI results in r4 are historical evidence, not relabeled r5 runs.

The final screenshot archive is
`docs/verification/rail/capture-20260906T062517Z/`: seven real PTY/xterm.js
screenshots plus raw bytes, cast, input records, request metadata, and binary
hashes. The parent inspected the four mandated 80×24 PNGs: no form remnants,
no corrupted final sentence, and the live-usage separator is correct.
The final reviewer ran the native word-oracle and UI-action tests and the
full 360-test suite, finding no remaining blocking production defect in the
reviewed scope. Earlier clipboard review also executed the non-draining-child
regression, rather than relying on code inspection alone.

### Local evidence (same frozen source, this machine)

- TUI locked offline all-target tests: **360 passed / 12 default-ignored** on
  stable and on MSRV Rust `1.85.0`, matching the independent r5 counts.
- TUI fmt / clippy `-D warnings` / rustdoc / `git diff --check`: pass
  (key source hashes match the final r5 source-hashes archive).
- Agent locked offline all-target tests: **263 passed / 2 ignored**, including
  the per-request usage identity tests (two sessions + per-request indexing),
  the 20-turn RPC soak (`tests/rpc_soak.rs`), plus fmt/clippy/doc/diff-check.
- Real-Agent loopback E2E: **11 scenarios pass** under the official serial
  command `--ignored --test-threads=1` (deterministic ~1 s locally; the parent
  r5 archive shows 0.52 s). A fully parallel spawn of the eleven real Agent
  processes is CPU/memory-contended on this laptop and can exceed a single
  bounded wait window; the failures are always harness wait-window errors
  (`recv timed out` / registration deadline), never product assertions, and
  scale with concurrency, which is why serial is the official command.
- Fixture integrity: **9 differential/fact/schema suites** run over the 114
  generated oracle cases plus provenance — this is intentionally **not** a
  claim of 114 point-to-point parity checks; full-cell comparison is carried
  by the component/snapshot suites and the Stage 7 capture.
- Fixed Rail fixture integrity/source-comparison: 9 tests pass over 114
  generated cases plus `PROVENANCE.json` (115 JSON files). The corpus is a
  reference-generated oracle, uncommitted by request.
- **Stage 7 real-terminal evidence** (final capture after the dictionary
  build: `docs/verification/rail/capture-20260906T062517Z/`, regenerated on
  the new word-library binary; the pre-CJK frozen-source r4 capture remains
  archived at `capture-20260906T055811Z/`; the regenerable `artifacts/` copy
  is git-ignored scratch and the earlier invalid bundle is labelled):
  - regenerated on the final binaries: the real Agent now emits per-request
    usage, and the raw PTY/footer in this capture shows the reported `↑`/`↓`
    counts next to `ctx ?` — visible live-usage evidence, not the model label;
  - capture mode: real PTY bytes -> @xterm/xterm 5.5.0 in headless Microsoft
    Edge via Playwright, element screenshots (no synthetic cells, no OS capture);
  - screenshots `01`–`04` cover mixed-loop running, same-loop completed,
    a single expanded section with its anchor retained, and multiline Editor
    plus one-row Footer at 80×24; `05`–`06` repeat completed/editor scenes at
    120×40, and `07` covers 62×18;
  - artifacts: `tui.pty.raw`, `tui.pty.cast.jsonl`, `inputs.jsonl`,
    `loopback.requests.jsonl`, and a compact `PROVENANCE.json` (git head/dirty
    counts plus a small status sample, never the full dirty listing) with
    binary SHA-256, node/xterm/playwright versions, fonts (Menlo/SF Mono 14px,
    line-height 1.12), and the per-checkpoint buffer-marker assertions;
  - the harness runs the real TUI and Agent binaries against a deterministic
    labeled loopback Responses model in an isolated git-initialized temp
    workspace on branch `dev`, and shuts down through the production Ctrl-C
    path.
- The driver (`tools/stage7-xtermjs/{driver.mjs,page.html}`) and the bridge
  (`scripts/stage7_xtermjs.py`) now replay recent PTY bytes to late-connecting
  viewers and run Edge with occlusion throttling disabled, so startup screens
  are never lost.

## RAIL Matrix

| ID | Result | Evidence / limitation |
|---|---|---|
| RAIL-01 | PASS | Current renderer snapshots and source fixtures show no rounded Editor frame, permanent two-row Footer, or normal-completion banner. |
| RAIL-02 | PASS | Shared Rail geometry tests cover one-cell gutter, one-cell rail, zero rail gap, and component-specific content insets. |
| RAIL-03 | PASS | Cell-style tests cover Rail slate, transparent sections, Thinking, User, and Tool state colors. |
| RAIL-04 | PASS | Editor height, cap, centering, and visible-window tests pass against fixed reference cases. |
| RAIL-05 | PASS | `EditorLayout` drives wrapping, cursor placement, click mapping, wide graphemes, and CJK tests. |
| RAIL-06 | PASS | Native threshold fixtures cover paste markers; raw submission and metadata-aware undo/redo tests pass. |
| RAIL-07 | PASS | Fixed Pi slash Enter fixtures and local completion tests pass. |
| RAIL-08 | PASS | Timestamp formatting, FIFO occurrence mapping, and unavailable timestamp behavior are tested. |
| RAIL-09 | PASS | Thinking threshold, hidden count, and manual fold persistence fixtures/tests pass. |
| RAIL-10 | PASS | Ordered assistant parts and cross-request reasoning/text tests pass. |
| RAIL-11 | PASS | Tool simple rows, write default folding, and 19/20/21-line boundaries are covered. |
| RAIL-12 | PASS | Model `bash` uses the green Tool surface; no `!bash` yellow styling is inferred from the name. |
| RAIL-13 | PASS | Bounded Agent display data and full received result expansion are tested. |
| RAIL-14 | PASS | Same-position release arbitration, drag protection, per-section toggles, a `pressedUrl` guard so link clicks never fold, and overlay-open clicks routed away from folding all pass. The guard now uses **real link geometry** from the markdown layout pass (`PreparedConversation::link_cells`) instead of a color heuristic, so links inside inline code or bold runs fold-guard correctly and same-colored non-link text cannot false-positive: `rail14_link_geometry_covers_code_and_bold_links_but_not_plain_text`. The staged fix also corrected a Markdown renderer style-merge bug that had hidden `md_link`/`md_code` colors (see RAIL-33 note). |
| RAIL-15 | PASS | Global Tool expansion, explicit per-tool overrides, and Thinking visibility/fold tests pass. |
| RAIL-16 | PASS | Stable section rebasing, follow-tail behavior, resize invalidation, and live/history updates are covered. |
| RAIL-17 | PASS | Geometry, edge mapping, release-only commit, cancellation, and 90 ms thumb interpolation tests pass. |
| RAIL-18 | PASS | Word/paragraph selection and edge auto-scroll pass, and CJK word selection now uses **real dictionary segmentation**: pinned `icu_segmenter` 2.1.2 (the ICU 2.1.x line already in this lockfile, no dependency upgrades; MSRV 1.83 ≤ 1.85) `new_auto` segments Han + Kana runs through the actual dictionary/LSTM models. It is locked to the **real pinned pi-tui 0.84.4 `TuiAltScreen.getWordSelection`** (no re-typed algorithm): `tools/reference_fixtures/cases/word_oracle.mts` calls the real prototype methods, and `tests/word_oracle.rs` compares **every display cell of a 40-line matrix — 498 word cells** — covering Chinese phrases (中华人民共和国→中华/人民/共和国, 南京市长江大桥→南京市/长江/大/桥, …), Chinese-English-number mixes, Japanese Kanji/Kana, decomposed combining marks, emoji ZWJ, paths, and all prior English cases. ICU4X output equals Node's `Intl.Segmenter` on every tested phrase. Scope is precisely bounded: `々`/combining-voiced-mark grouping and Thai/Lao/Khmer/Myanmar dictionary scripts are not in the matched set (concrete divergences, documented); the claim is exact for the tested Chinese/Japanese/localized-English corpus, not "all locales and all dictionary versions forever". |
| RAIL-19 | PASS | Copy ranges exclude rails/padding/ANSI, native clipboard adapters are injectable, and 1800 ms feedback is tested. |
| RAIL-20 | PASS | One-row Footer order, colors, right alignment, branch/model shortening, and narrow fitting pass. |
| RAIL-21 | PASS | Number, cost, context, duration, timestamp, and fitting boundary tests pass. |
| RAIL-22 | PASS | Per-request usage is now surfaced live: the Agent's model-stream wrapper caches each real `Usage` observation keyed `(loop_id, request_index)` and emits a read-only `request_usage` event; the TUI stores it per request and the footer shows the known `↑`/`↓` total while the loop is still running (no fake zero), then the persisted loop total replaces the live rows instead of summing, history pages of older loops stay untouched, unsaved/failed rows sit in a separate bucket, reasoning tokens are never counted as output or cache-write, and nullable metrics render as `usage ?`. Verified by Agent identity tests (two sessions + per-request), TUI projection tests, and a real-Agent gated E2E. Context/cost remain `Unknown` by design (no pricing source; RAIL-23). |
| RAIL-23 | PASS | Context, cost, subscription, and missing fields remain unknown or omitted; no values are fabricated. |
| RAIL-24 | PASS | Agent presentation formatter, live/history display DTOs, ordered parts, timestamps, and current E2E tool flow pass. |
| RAIL-25 | PASS | Agent wrapper tests preserve tool results, cancellation, deadlines, and loop ordering. |
| RAIL-26 | PASS | Shared-model/session identity tests exist in Agent, and the new real-Agent stress scenarios cover background generation after a second-tool expand and session-switch preserving the fold. |
| RAIL-27 | PASS | Optional DTO fields, old history compatibility, and redacted Debug tests pass. |
| RAIL-28 | PASS | Eleven real-Agent loopback E2E scenarios cover multi-request, Tool, Steer, update, next-turn, shutdown, 6-loop/10-request stress, expand-under-generation, session-switch, and live per-request usage paths. |
| RAIL-29 | PASS | App-flow and Agent tests cover event gaps, history reconciliation, and persistence failure retention. |
| RAIL-30 | PASS | Blocked result retention and close verification regression tests pass. |
| RAIL-31 | PASS | Presentation, protocol, event, approval, and log tests reject sensitive Debug/log leakage. |
| RAIL-32 | PARTIAL | Native Linux (final r5) plus local macOS and Windows GNU cross (clippy `-D warnings` + `--no-run` link) all pass on the frozen source, and the real-PTY restore test passes under a TTY. Recorded PARTIAL because none of these ran on a hosted CI service with a scheduled cross-platform matrix, and the real-PTY case remains ignored outside a TTY. |
| RAIL-33 | PARTIAL | The 114 generated cases are a **schema/fact subset**, not 114 parity checks: `minicore-rail-cell-v1` validates well-formed cell grids, and 9 differential/fact/schema suites compare a style/fact subset (markdown/link `md_link` 129,162,190 == pinned Pi oracle, inline-code, user/tool surfaces) against the pinned source. Full-cell comparison is carried by the component/snapshot suites and the real-terminal Stage 7 capture. Recorded PARTIAL because the corpus is reference-generated, uncommitted, and the differential coverage is intentionally limited rather than a full 114-case point-to-point parity suite. |
| RAIL-34 | PASS | The authoritative archive `docs/verification/rail/capture-20260906T062517Z/` holds the four mandated long/short screenshots plus three bonus ones, the raw PTY/cast/inputs/loopback-request logs, and the compact PROVENANCE with binary SHA-256 and per-checkpoint buffer-marker assertions. The parent read all four 80x24 PNGs from the archive: clean, no stale form ghost, exact final sentence intact, and the partial-usage `usage ?` separator visible. The bundle's loop identity is **inferential within the capture** — one continuously-running session whose two logged Responses requests belong to the visible single loop — explicitly **not** claimed as an in-bundle RPC proof. The hard same-loop identity is a separate, clearly distinguished item: the real-Agent E2E `e2e_scenario_e_same_loop_update` asserts the actual `loop_id` string equality across the mid-loop update via the Agent RPC (requests/tool_rounds/history all keyed to one loop). The `artifacts/` copies are regenerable scratch (git-ignored); the archive in `docs/` is the cited record. |

## Source -> Target Coverage Table

Each pinned Rail/Pi behavior maps to the TUI/Agent code and tests that
implement it.

| Source behavior (pinned) | Target implementation | Verification |
|---|---|---|
| One-cell rail gutter + zero gap | `src/ui/rail.rs` (`APP_GUTTER_WIDTH`) | `rail_fixtures.rs: rust_surface_primitives_match_source_rail_and_content_geometry` |
| One-row footer, right-aligned status | `src/ui/footer.rs` | footer component tests + `04-multiline-editor-footer.png` |
| Editor height/cap/centering formula | `src/ui/editor_layout.rs` | `rust_editor_height_formula_matches_source_terminal_sizes` |
| Paste threshold >10 lines / >1000 chars | `src/state/composer.rs`, editor paste path | `paste/*` fixtures in `rail_fixtures.rs` |
| Slash `Enter` keeps `skill:` commands | `src/keymap.rs`, completion | `rust_slash_enter_states_match_the_fixed_native_editor` |
| `formatUserMessageTimestamp` (en-US, local timezone; fixture uses UTC) | `src/ui/user.rs::format_user_timestamp`; Agent acceptance-time metadata | User timestamp fixtures + Agent FIFO/reopen tests |
| Thinking collapses when rawLines > 3 | `src/ui/reasoning.rs` (`reasoning_lines_with_fold`) | `rust_reasoning_fold_rows_match_source_threshold_fixtures` |
| Model-tool collapse policy and `executionHiddenLineCount` | `src/ui/tool.rs::default_expanded`; Agent display line counts | Real model-tool threshold fixtures; user `!bash` fixtures are separate |
| `fitAligned` narrow takes right group | `src/ui/footer.rs::fit_aligned` | Footer narrow tests and `07-narrow-final.png` |
| Expanded tool keeps anchored rows | `src/ui/transcript.rs`, section rebasing | `03-card-expanded-anchor.png` + stress E2E scenarios |
| `Intl.Segmenter` word granularity + `/ -` joiners + CJK dictionary | `src/app/ui_actions.rs::word_cell_bounds` + `icu_segmenter` 2.1.2 (`new_auto`) | `tests/word_oracle.rs` (real pi-tui 0.84.4 `TuiAltScreen.getWordSelection`, 498 word cells incl. Chinese/Japanese) + RAIL-18 tests |
| Link click cannot fold (`pressedUrl`) | `src/app/ui_actions.rs::pressed_cell_is_link` + App `mouse_pressed_on_link` | `app::tests::rail14_link_and_overlay_clicks_never_fold_while_plain_click_toggles` |
| Markdown link/code colors | `src/markdown.rs` (`base.patch(seg.style)` merge order) | RAIL-33 `markdown/link.json` + `markdown/inline-code.json` |
| Context/cost/subscription unknown | Agent `presentation.rs` (`ContextKind::Unknown`, null cost) | RAIL-23 tests |
| Per-request live usage (spec 9.4/12.4) | Agent `PresentationModel` stream wrapper + `AgentEvent::RequestUsage`; TUI `SessionView::live_request_usage` merge | Agent `request_usage_*` tests, TUI `live_per_request_usage_*`, real-Agent E2E `e2e_live_request_usage_*` |
| Word/paragraph native selector | `src/app/ui_actions.rs::word_cell_bounds` | `tests/word_oracle.rs` (real `TuiAltScreen.getWordSelection`/`getLineSelection`) |
| Old JSONL without timestamps loads with `None` | Agent `store.rs` (`user_times` Option, `skip_serializing_if`) | store tests incl. invalid/too-many timestamp cases |
| Live tool display == history display | Agent single formatter `build_tool_display` | `live_tool_presentation_and_result_keep_runtime_identity` |
| Model identity fixed at request boundary | Agent `PresentationModel` / `PresentationTool` wrappers | `model_swap_keeps_request_identity_for_live_tool_display`, `running_model_update_*` |

## Known Limits

The Agent has no reliable provider context-metering or pricing source, so the
context `ctx ?`, omitted cost, and unknown subscription are intentional and
never fabricated. Old history without stored acceptance metadata remains
unavailable. The TUI does not add Subagent, MCP, Skills execution, approval,
compaction, `!bash`, or other out-of-scope product features. The Runtime stays
untouched (HEAD `6cd2bdbc`).

Intentional differences follow the spec's precedence over the fixed source:
- User content uses the specified `#313244`; the pinned native Markdown
  content cells can retain `#343541`.
- Cancelled tools use the specified `#292A35` background and `#7F849C` rail.
  The pinned renderer leaves cancellation pending-shaped; its
  `state-cancelled-as-pending.json` oracle is retained separately.
- Generic detail is explicitly bounded and whitelisted. It does not expose
  the full invocation JSON used by Rail's `compactArgs`, even when the Agent
  has those arguments. Missing detail uses the tool-name fallback.
- Unknown historical timestamps display `time unavailable`, not a new local
  timestamp. Context and billing metrics remain unknown or omitted.

Screenshots use the recorded Menlo-family terminal configuration and
`#2D2A2E` background. They verify actual terminal output, not pixel identity
with the user's differently sized screenshot or unrecorded font settings.

Clipboard: the adapter is a single compile-time-selected native program
(`pbcopy` on macOS, `xclip` on Linux, `clip` on Windows) with no fallback
chain. The write and the wait share one wall-clock deadline: the payload is
streamed by a controlled writer thread, and a child that never drains its
pipe is killed and joined (the `EPIPE` from the broken pipe unblocks the
writer), so a non-draining child can never hang the TUI regardless of payload
size. This is locked by a real regression test (1 MiB payload into a
non-reading child, bounded return) plus a draining-reader success test and a
nonzero-exit cleanup test. macOS `pbcopy` is the only adapter exercised on
real hardware here; the Linux `xclip` and Windows `clip` (UTF-16LE+BOM payload,
the only encoding `clip.exe` round-trips) branches are compile-tested only and
documented as such, not claimed as native-machine-verified. The MockClipboard
used by App/UI tests never spawns a clipboard program.

The native word selector uses `Intl.Segmenter(undefined, { granularity:
"word" })` plus the pinned `/`-`-` terminal-word joiners. The Rust
implementation preserves grapheme/cell safety and is locked to a native oracle
that executes the real pinned algorithm (see RAIL-18), now with **ICU4X
`icu_segmenter` 2.1.2 dictionary segmentation** for CJK. Exact-match scope:
Chinese (dictionary words), Japanese Kanji + Kana (via `new_auto`), and the
existing localized-English corpus. Concrete non-targets (both models split
differently from Node on these, or differ between models): `々` / combining
voiced marks, and the Thai/Lao/Khmer/Myanmar dictionary scripts. The oracle
fixture records every cell; the claim is bounded to the tested corpus and
does not promise other locales or future dictionary versions.

Resolved verification defects are not accepted differences: the earlier
capture-side form remnants were fixed with renderer synchronization and
strict full-sentence/box-glyph assertions; the invalid bundle is marked as
such. Final screenshots do not mask corrupted text. The Markdown style merge
was corrected to `base.patch(seg.style)` and source-fixture tests preserve
link/code colors.

The Stage 7 harness (`scripts/stage7_xtermjs.py`) runs from a normal terminal
without elevated permissions; Edge is launched headless via the pinned
playwright-core driver, so no Screen Recording permission is involved.
## Delivery Identity

Every repository below is at the states actually used during this parity work.
No commit was created by this work; all endpoints were advanced by the user's
own commits (or unchanged).

| Repository | Original start HEAD | End HEAD | Change owner |
|---|---|---|---|
| `minicore-tui` | `2b8268dbba81c162b30e984b9b31a58ebc3bba65` | unchanged (`2b8268d`) + uncommitted parity work | this work |
| `minicore-agent` | `b2e23938d073ab21c2775faa623561ba929a5ed1` | user committed mid-task to `2d16f554796861a21a49afcd77f4eab74022bf92`; our presentation work is on top, uncommitted | user commit + this work |
| `minicore-runtime` | `87f3cf92b9b5980b0f468174a319cf53427d858e` | user committed to `6cd2bdbc634437dea925495c61c7eb0be10ba171` | user commit only; never modified by this work (diff clean apart from the user's own untracked spec files) |
| `pi-rail-ui` development checkout | `86c6fe96b59ac07e4c4e649aaa974ef8bcb1723e` | user advanced to `395ee40af5bf5d89283b8dd9d19e86c1ae198bef` during the work | user commit |
| `pi-rail-ui` fixed reference | `1d0dd1611a4d9546c64fe9f5b5c966253fb88eba` (Pi `0.84.4`) | unchanged, clean | none |

## Modified Production Files

Compact inventory of the production source this parity work actually touched,
grouped by repository (test-only files and generated fixtures are not listed
here).

TUI production files (brace groups enumerate individual files):
- `src/{app,clipboard,command,event,keymap,lib,main,markdown,protocol,theme}.rs`
- `src/app/ui_actions.rs`
- `src/state/{composer,mod,selection,session,tool,transcript,turn,view}.rs`
- `src/ui/{assistant,composer,editor_layout,footer,header,layout,mod,rail,reasoning,scrollbar,tool,transcript,user}.rs`

New TUI production modules are `app/ui_actions.rs`, `clipboard.rs`,
`state/view.rs`, `ui/editor_layout.rs`, `ui/rail.rs`, and `ui/scrollbar.rs`.
`Cargo.toml`/`Cargo.lock` add pinned segmentation/time dependencies; existing
Ratatui, Crossterm, and tui-textarea versions are unchanged.

Agent production files:
- `src/{agent,event,history,lib,presentation,sessions,store,workspace}.rs`
- `src/rpc/{protocol,server}.rs`
- `src/tools/mod.rs`

`src/presentation.rs` is the new Agent module; the existing modules integrate
its optional read-only fields, wrappers, and timestamp metadata.

Runtime (`minicore-runtime`): **none**. Its diff is clean except the user's
own untracked specification files; this work never modified it.

## Preserved Paths

`RpcProcess`, request-ID dispatch, `App::update` as the single state writer,
Loop/Request identity, Prompt/Steer submission and revision guards,
request-boundary model updates, paginated History reconciliation, retired
loop fences, Blocked/Unsaved result retention, close verification, and
`TerminalGuard` cleanup remain in place. The TUI does not link Agent/Runtime
crates, read their Store/Workspace, or execute Agent tools. No Subagent engine,
plugin/theme framework, second execution loop, or follow-up queue was added.

The user's own materials and the parity evidence are all preserved: the
reference checkout (`/Users/zzq/Develops/pi-rail-ui-ref-r1`) and development
checkout, the Rail spec, the pinned reference fixtures generator, the
authoritative capture archive under `docs/verification/rail/`, and the user's
dirty work (Agent/Runtime user commits and untracked spec files). Only
this work's own scratch was removed (`src/zwj_probe.rs`, `scripts/__pycache__/`)
and the regenerable `artifacts/` output is git-ignored rather than deleted, so
prior invalid bundles remain reproducible on disk. No user PAT, provider
credential, or host password was written to the repository. Test configuration
contains only synthetic loopback keys.
