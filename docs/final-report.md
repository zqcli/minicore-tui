# Historical Final Report: Agent v0.3 / Runtime v0.4 Migration

This report preserves the earlier 0.2.x delivery record. The current release
line is **TUI 0.3.0 / Agent 0.5.0 / Runtime 0.4.1**; see
[`docs/refactor-acceptance.md`](refactor-acceptance.md),
[`docs/backend.md`](backend.md), and [`docs/performance.md`](performance.md)
for the current refactor evidence. The
historical report begins with the older release versions **TUI 0.2.8 / Agent 0.3.3**.
The original [0.2.8 release](release-0.2.8.md) was a paired version update without additional
TUI behavior changes. See
[release-0.2.7.md](release-0.2.7.md) for buffered terminal output, level-1 Debug
package optimization, preserved executables/cache cleanup, and remote-only builds.
Paired-release checks are in [verification/0.2.8](verification/0.2.8/README.md);
performance evidence remains in [verification/0.2.7](verification/0.2.7/README.md).
The [0.2.6 changes](release-0.2.6.md) retain dependency-level optimization, and the
[0.2.5 changes](release-0.2.5.md) retain history-layout reuse, bounded visible-row
painting and coalesced preparation.
The [0.2.4 changes](release-0.2.4.md) retain true reasoning-part boundaries,
paired Steer receipts and the gray FIFO queue above Working. Runtime remains untouched.
The current 0.3.0 core code/test/snapshot baseline is
`0aa64c5e4d9211351123db059547beddb15c2cce`; F-review remediation is `daa944a`.
It is paired with Agent `061743369459299e66be97bf97d2b27352a39914` and Runtime
`6cd2bdbc634437dea925495c61c7eb0be10ba171`. Authorized remote Rust 1.85/stable
F-review checks each pass 830 tests with no failures and 48 ignored; the
isolated fixed backend job passes 34/34 serial loopback E2Es; and the current
Release performance set passes 7/7. Linux kernel-PTY lifecycle/input/resize/
shutdown evidence passes, including 30-second idle draw evidence. The older
local rustc 1.98.0 run is disclosed as an execution deviation only. Hosted CI,
native/manual iTerm2/IME, exact allocator/RSS, provider, and real oversized-
Agent checks remain separate and are not claimed by this historical report.

The [0.2.3 Footer fixes](release-0.2.3.md) remain, but its pending-Steer User-card
UI is superseded by the 0.2.4 queue. The [0.2.2 fixes](release-0.2.2.md) cover
spacing and extended reasoning levels. Earlier verification remains historical.

The [0.2.1 package](release-0.2.1.md), Rail gates below, and **0.2.0** migration
evidence are historical; their counts and cross-platform checks are not
relabeled as new runs. Agent/Runtime protocol compatibility is unchanged.

## Historical 0.2.x Follow-Ups

The same-version [Tool/reload/subagent follow-up](verification/followups/README.md)
adds safe failure bodies, 100 ms Working animation, consecutive User spacing,
empty/new startup guidance, bounded Codex Update File support, `/reload`, and
native stateless subagent single/parallel/chain. Its TUI `30ea7ca` pairing is now
superseded by the [public reload correction](verification/reload-refresh/README.md):
current sources are TUI `a604e55` / unchanged Agent `f1697f7`. The correction adds
the omitted exact-turn result reread and prevents old wait results or send failures
from overwriting a newer turn or clearing its queue handoff. Three real REDs and
separate test setup failures are retained. Persistent orchestration and compaction
are not implemented.
Remote stable/MSRV each passed **508 TUI tests / 19 ignored** and separately
**18 E2E tests**; the unchanged Agent retains its earlier **369 passed / 2 ignored**
acceptance. All six native Debug/Release workflows and real-TTY checks were rerun.
Corrected TUI binaries/symbols are installed with old inodes preserved; Agent
bytes are unchanged. No user process restart, new push, hosted CI, release tag or
pixel parity is implied.

## Historical Session Acceptance

Session management now shares dock-panel geometry with all selectors, New
Session, Help and Logs. Session-specific actions provide open, new, refresh,
rename, close and explicit default-Cancel permanent deletion. Selection and
responses retain stable Session IDs; unknown lifecycle state, incomplete history
and retained unsafe results block destructive shortcuts. History requests capture
the gap revision at issuance, and deleted-session tombstones reject late replies.
Rename uses the separately committed Agent `session.rename` API and updates the
UI only after its complete ACK. Runtime, dependencies and package versions remain
unchanged.

The previous Session acceptance rebuilt a fresh remote source mirror: stable and
Rust 1.85.0 each passed **464 tests with 18 default ignored**; the 17 real-Agent E2E
cases then passed explicitly. Agent passed **318 tests with 2 ignored** on both
toolchains. Stable strict Clippy, format checks and warning-denied rustdoc passed.
MSRV Clippy's pre-existing diagnostics are not claimed as fixed. Earlier review
stages are retained under `verification/stage3-session-panel/` and
`verification/stage4-session-panel/`; current counts are in [testing.md](testing.md).
Final [native acceptance and installation](verification/session-management/README.md)
passed for both Debug and Release, including file-prompt snapshots, filtered
Session targets, default-Cancel deletion, the existing streaming/FIFO workflow,
and real-PTY normal/panic restoration. The native run found a hidden-selection
bug; its fixes and original failing evidence are retained. Accepted binaries
and Debug symbols were installed at that boundary with old executable inodes
preserved; the current follow-up installation supersedes those paths. User
processes were not restarted; no hosted CI, push or pixel-parity claim is implied.

## Historical Rail Follow-Up Status

The source tree also contains the Rail Stage 0–7 follow-up described
in [rail-ui-parity-stage0.md](rail-ui-parity-stage0.md) and summarized in
[rail-ui-parity-report.md](rail-ui-parity-report.md). Its fixed source oracle is
Rail `1d0dd1611a4d9546c64fe9f5b5c966253fb88eba` with Pi `0.84.4`;
`minicore-runtime` remains untouched. This section records the pre-0.2.2 Rail
verification, not the current patch gates, and does not claim complete Rail
product parity.

- Agent presentation support is bounded, read-only, identity-keyed, and
  redacted from `Debug`; it covers Tool display facts, ordered assistant parts,
  acceptance timestamps, and `session.presentation` without changing execution
  outcomes or cancellation/deadline behavior.
- TUI Stage 2–5 covers shared Rail geometry, the one-row footer, the fixed
  editor surface, User/Thinking/Assistant/Tool presentation, stable section
  ranges, per-section fold state, order-safe Tool result fallback, timestamp
  formatting, mouse folding, anchored selection/copy, clipboard feedback,
  scrollbar live body preview/release commit, native paste projection, slash completion, native
  Editor click cases, RAIL-14 click arbitration with **real link geometry**
  (link/overlay clicks never fold; inline-code/bold links are links, plain
  text is not), and a word-selection model locked to the **real pinned Pi
  0.84.4 fullscreen word selector** via the native oracle in
  `tools/reference_fixtures/cases/word_oracle.mts` + `tests/word_oracle.rs`. The generated corpus
  contains 114 cases in the worktree; it remains uncommitted by request.
- Historical Rail gates: TUI default all-target tests **360 passed / 12 ignored**
  (and the same 360 on MSRV Rust 1.85), Agent all-target **263 passed / 2
  ignored** including the extended 20-turn RPC soak, and **11 real-Agent
  loopback E2E scenarios pass** under the official serial command
  (`--ignored --test-threads=1`; parallel spawn of eleven real Agent processes is
  CPU/memory-contended and can exceed a single bounded wait window, so serial
  is the official run — failures in parallel are always harness wait-window
  errors, never product assertions). The new E2E scenario proves per-request
  usage shows in the footer while the loop is still gated and the persisted
  loop total replaces it after history reload (spec 9.4/12.4). Rustfmt, locked
  offline check, Clippy `-D warnings`, rustdoc, and `git diff --check` pass in
  both repositories.
- Stage 7 real-terminal evidence is present: `scripts/stage7_xtermjs.py` and
  the pinned xterm.js/Playwright driver capture real PTY bytes through a real
  terminal emulator in headless Edge. The parent-inspected r4 archive is
  `docs/verification/rail/capture-20260906T055811Z/`; the final capture after
  the CJK dictionary build is archived locally at
  `docs/verification/rail/capture-20260906T062517Z/`. Both hold the four mandated
  screenshots: mixed-loop running, same-loop completed, single-card expanded
  with anchor, multiline editor + single footer — plus three bonus shots —
  along with raw PTY, cast, inputs, loopback-request logs, and a compact
  `PROVENANCE.json` with binary SHA-256 and font/geometry provenance). The
  archived copy passed per-checkpoint buffer-marker assertions and the parent
  read all four 80x24 PNGs clean. The regenerable `artifacts/` copies are
  git-ignored scratch. See the RAIL matrix (RAIL-34 PASS with its in-capture
  identity scoped as inferential and the hard loop_id as a separate E2E RPC
  assertion; RAIL-33 PARTIAL because the fixture corpus is
  reference-generated, uncommitted, and only a 9-suite differential/fact/
  schema subset) and the Source -> Target Coverage table in the parity report.

Independent final TUI verification is archived at
`docs/verification/rail/final-r5/`: native Linux stable and MSRV 1.85 each
**360 passed / 12 ignored**, 11 serial real-Agent E2E scenarios (0.52 s),
real-PTY restore (1 pass), Windows GNU cross-clippy and all-target test linking
(`--no-run`), plus rustfmt/clippy/rustdoc. Key source hashes match the final
local tree, including dictionary-based CJK selection. The unchanged Agent's
independent **263 passed / 2 ignored**, quality checks, and 20-turn soak
remain archived in `final-r4/`; they are not relabeled r5 runs. Neither
cross-compilation nor these direct machine runs are hosted CI evidence.
The authoritative final screenshot archive is
`docs/verification/rail/capture-20260906T062517Z/`; the parent inspected all
four mandated 80×24 PNGs and confirmed them clean.

Remaining parity work is reported rather than hidden: CJK word selection is
now real dictionary segmentation (icu_segmenter 2.1.2 `new_auto`, matching
the pinned Pi `Intl.Segmenter` on the tested Chinese/Japanese corpus, cell by
cell), with the scope precisely bounded — `々`/combining-voiced-mark and the
Thai/Lao/Khmer/Myanmar dictionary scripts are documented non-targets and do
not make this an "all locales" claim. Still partial: a hosted cross-platform
CI service (native Linux plus macOS and Windows-GNU cross were run, but not
on a scheduled CI host; RAIL-32) and the uncommitted fixture corpus
(RAIL-33). The PTY-restore case remains ignored outside a TTY. Context, cost,
subscription, unavailable old timestamps, and missing branch values remain
explicit unknowns where the Agent has no reliable source. No commits were
created by this work.

## Historical Delivery Identity

| Repository | Original start HEAD | End HEAD | Change owner |
|---|---|---|---|
| TUI | `2b8268dbba81c162b30e984b9b31a58ebc3bba65` | unchanged + uncommitted parity work | this work |
| Agent 0.3.0 | `b2e23938d073ab21c2775faa623561ba929a5ed1` | user committed mid-task to `2d16f554796861a21a49afcd77f4eab74022bf92` + our uncommitted presentation work | user commit + this work |
| Runtime 0.4.0 | `87f3cf92b9b5980b0f468174a319cf53427d858e` | user committed to `6cd2bdbc634437dea925495c61c7eb0be10ba171` | user commit; never modified by this work |
| pi-rail-ui (dev checkout) | `86c6fe96b59ac07e4c4e649aaa974ef8bcb1723e` | user advanced to `395ee40af5bf5d89283b8dd9d19e86c1ae198bef` during the work | user commit |
| pi-rail-ui (fixed reference) | `1d0dd1611a4d9546c64fe9f5b5c966253fb88eba` (Pi `0.84.4`) | unchanged, clean | none |

The recorded repository heads above are the actual local provenance for this
parity work; see the parity report for the compact modified-production-file
inventory and preserved-paths summary.
- Migration baseline package: `0.2.0`; supported wire range: Agent `0.3.x` only
- Agent presentation/RPC source is modified as described above; Runtime source
  was not modified. No commit was created.

## Historical Implemented Semantics

- `src/protocol.rs` models the v0.3 JSON-RPC DTOs: `TurnRef`, indexed
  history, five session states, direct `turn.wait` results, outcomes,
  persistence, usage, request-indexed events, `session.update`, and
  `turn.steer`. Version gating accepts `0.3.x`; release prerelease behavior is
  tested through `cfg!(debug_assertions)`.
- `src/rpc.rs` owns one stdin writer, one stdout reader, one stderr reader,
  bounded frames/logs, request correlation, child reaping, and shutdown drain.
- `src/app.rs` is the only reducer entry point. Requests are registered before
  commands leave `App::update`, and responses/events are routed by request ID,
  session ID, loop ID, request index, and session-state query token.
- `src/state/turn.rs` separates one `LiveLoop` from its multiple
  `LiveRequest` segments. Model A → Tool → Model B remains one loop while
  deltas and tools stay attached to their request index.
- `src/state/session.rs` retains per-session live output, history projection,
  `last_result`, blocked/unsaved state, pending configuration evidence, event
  gaps, background-session isolation, and a bounded retired-loop lifecycle
  fence.
- History is the durable authority. `continue_history_chain()` advances by
  contiguous raw item offsets, detects gaps/conflicts, and never fabricates a
  terminal history item. Live output that arrives before `turn.send` response,
  or tool progress that arrives before `tool_started`, is retained with an
  explicit gap/unknown marker.
- Loaded/running sessions reuse `activate_existing_session` instead of sending
  a redundant `session.open`. True close→reopen establishes a bounded retired
  loop fence before the request, preserves it on open failure, and invalidates
  old request ids only after a successful open.
- Late events from a completed loop cannot bind a new prompt when the new
  `LiveLoop` has no reference; close→reopen invalidates old session requests,
  and close verification treats only Agent `SESSION_NOT_LOADED` as proof of
  unload. Connection loss and forced shutdown expose unknown result/save state
  without fabricating a failure.
- `turn.wait` preserves `completed`, `cancelled(reason)`, and
  `failed(kind/model_error)` independently from `persisted` or `persistence
  failed`. Persistence failure retains `last_result`, `LiveLoop`, `UnsavedLoop`,
  the original `TurnRef`, and blocked status without claiming durable history.
- `src/ui/status.rs`, `src/ui/footer.rs`, and `src/ui/transcript.rs` display
  the same result facts: `completed`, `cancelled (reason)`, or
  `failed: kind[: model error]`, plus `persisted` or `persistence failed`.
  Unknown and shutdown cancellation reasons are visible; missing live model
  configuration is shown as `config unknown`.
- Composer input is bounded at 256 KiB of UTF-8 bytes and carries a monotonic
  edit revision for delayed steering acknowledgements. Slash commands remain
  local, running sessions route normal text to steering, and selector updates
  use request-boundary semantics without silently downgrading reasoning.
  `session_opened` notifications only create unknown views or trigger missing
  state reads; they do not overwrite existing SessionInfo or regress a live
  loop. Forced shutdown uses `RpcProcess::terminate_with_observer` to drain
  late frames and stderr after `Exited` within a bounded deadline, without
  dispatching new RPC commands; reports combine captured stderr with
  known/unknown result facts.

## Historical MIG Coverage

The complete one-row-per-criterion matrix is in `docs/acceptance.md`; the
16-method RPC audit is in `docs/rpc-contract.md`.
MIG-001 through MIG-032 cover pins, protocol DTOs, version gates, and errors;
MIG-033 through MIG-054 cover indexed history and multi-request live routing;
MIG-055 through MIG-077 cover configuration updates and steering;
MIG-078 through MIG-096 cover persistence, blocked sessions, close, reopen,
and shutdown; MIG-097 through MIG-113 cover the UI; MIG-114 through MIG-121
cover RPC, terminal, and dependency boundaries; MIG-122 through MIG-140 cover
flows, snapshots, E2E, and platform CI; MIG-141 through MIG-160 cover the r2
backend revisions and their edge cases.

The historical matrix reports **157 PASS / 3 NOT RUN**. The parent independently
verified pins, backend build provenance, dependency absence, and evidence
recording (MIG-001/002/006/007/141/160) by source/metadata audit, which is
appropriate for those requirements and is not a runtime SHA-attestation claim.
MIG-138/139/140 are NOT RUN: GitHub Actions Linux/macOS/Windows jobs were not
triggered. Remote Linux tests and cross-target checks are not substituted for
CI. The current Protocol v1 acceptance matrix is
[`docs/refactor-acceptance.md`](refactor-acceptance.md).

## Historical Final6 Verification

All final6 commands ran remotely in
`/root/minicore-tui-r2-01a06ec1/tui` on `192.168.20.199`. Raw final2, final3,
final4, final5, and final6 post-review logs are in
`/root/minicore-tui-r2-01a06ec1/logs/final[23456]-*`.

- MSRV 1.85 and stable `cargo test --locked --all-targets`: each passed 273
  tests with 0 failures and 8 default ignored tests (197 library, 8 main, and
  49 app-flow tests).
- Stable release version gate: 2 passed, including the conditional
  prerelease policy.
- Stable clippy with `-D warnings`, stable/MSRV rustfmt, and rustdoc with
  `RUSTDOCFLAGS=-D warnings`: passed with no warnings.
- Snapshot generation/check: 47 passed; close, unsaved, unknown-cancel, and
  shutdown-cancel snapshots are present.
- Real remote PTY restore: 1 ignored test passed with ANSI terminal restore
  evidence.
- Real Agent loopback E2E: 7 passed against the pinned Agent binary, covering
  discovery, basic turn, tools, steering, same-loop update, next-turn update,
  and shutdown cancellation.
- Dependency tree checks passed and show ratatui `0.29.0` and crossterm
  `0.28.1`.

## Platform Evidence

GitHub Actions Linux, macOS, and Windows jobs were not run. On the final6 source,
the parent remotely passed `cargo +stable check --locked --offline --all-targets`
for both `x86_64-pc-windows-gnu` and `x86_64-apple-darwin`. These are compile/type
checks, not native tests or CI; no local Rust build was performed.

## Retained And Removed Modules

Retained and adapted: `src/terminal.rs`, `src/theme.rs`, `src/markdown.rs`,
`src/state/composer.rs`, `src/state/selection.rs`, `src/ui/` presentation
components, Ratatui snapshots, RPC I/O tests, and terminal restore tests.

Rewritten in place: the old TUI v0.1/Agent v0.2 Protocol DTOs, Transcript
projection, and flat LiveTurn reducer. Removed concepts: `instance_id`,
`ConversationSeq`, `session.transcript`, durable terminal items, unfinished-turn
repair assumptions, immutable model settings, and guessed tool arguments.
No tracked source file was deleted: `state/transcript.rs`, `state/turn.rs`, and
`ui/transcript.rs` retain their filenames but implement the new History/LiveLoop
semantics. Agent/Runtime Rust dependencies were absent before and remain absent;
no dual-protocol adapter or local Store migration was introduced.

## Five Backend Commits

The five commits below are provenance and semantic inputs, not TUI changes:

1. `bac2b715f7bee3a5865fc581f133dd60acadd1bc`: blocked completion is retained;
   TUI wording is “the previous result remains available while loaded.”
2. `e511d9e29c75f7d6a7476baec09fc55ca5fcd379`: same-loop request-boundary
   updates; TUI wording preserves one `loop_id` and request-indexed output.
3. `cc9ddf7436b49d2360ce5fde16b76e81cd52ef92`: persisted session settings are
   validated; TUI says data may be unavailable, invalid, or unsupported rather
   than diagnosing a generic Store error as an old format.
4. `c362446a156dbcc5854930d0dbaac97bb612ba19`: append, tail repair, shutdown,
   and cancellation boundaries; TUI says `persisted` is current-process append
   confirmation, not transaction/fsync/crash durability.
5. `b2e23938d073ab21c2775faa623561ba929a5ed1`: bounded Agent write-test
   gates; TUI final6 helpers use deadlines and cleanup rather than unbounded
   waits.

## Wording Audit

- Saving: `persisted` confirms Agent's current-process append, not a transaction,
  fsync, crash durability, or atomic tool side effects. The UNSAVED banner says
  the Agent did not confirm saving; it does not assert the disk is empty.
- Closing: `reason=user` is accepted as the actual close/shutdown cancellation
  reason. Shutdown success does not override known persistence failure; forced
  termination preserves known results and reports unknown result/save state.
- Store: a generic error says data may be unavailable, invalid, or from an
  unsupported format. It is not diagnosed as definitely old format or repaired.

## Capability Gaps And Unrun Tests

- GitHub CI and native macOS/Windows execution: no jobs triggered/native remote
  runners available; only Linux execution and cross-target checks are claimed.
- Real external LLMs: intentionally not used; the pinned Agent E2E uses an
  isolated loopback mock and no real credentials.
- Production append-failure/worker-panic injection and power-loss durability:
  not injected into the production binary; no debug RPC was added. TUI fixtures
  and the pinned Agent's blocked/library/RPC tests cover the supported contract,
  not every filesystem failure or crash-recovery combination.
- Unfiltered all-platform offline metadata initially failed on a missing cached
  Redox-only package; Linux-filtered metadata was used instead. No Redox build
  or test is claimed.
- Reopened History has no historical outcome/terminal registry; tool arguments
  are not exposed; repeated wait cannot recover missing live text or retry save.
- Approval, Compaction, Plugin, MCP, Subagent, automatic reconnect/restart, and
  local Store migration remain intentionally out of scope.

The v0.1 spec is marked superseded. The earlier pre-r2 v0.2 spec is also declared
superseded in the migration notes; its original file was not supplied in this
checkout. Independent implementer/reviewer iterations finished with no remaining
review findings; this does not replace unrun platform or fault-injection tests.
