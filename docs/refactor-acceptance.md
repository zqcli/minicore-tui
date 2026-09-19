# v0.3 Refactor Acceptance Matrix (REF-01…REF-56)

Status values are deliberately strict:

- **Passed** — the complete criterion has named coverage and the recorded
  validation passed.
- **Failed** — the current tree demonstrably violates the criterion.
- **Not run** — the criterion is missing, only partially covered, or its final
  validation has not been run on the current tree.
- **Not applicable** — the criterion does not apply to this project.

## Current Evidence

The authoritative builder is `root@192.168.20.199`, workspace
`/root/minicore-tui-v03-refactor/tui`, with Rust 1.85.0. The current C2c
mainline includes the bounded serialized canonical-item decode worker, shared
history owners, monotonic ToolFacts presentation ownership, content-based
ScrollAnchor restoration, viewport/neighbor/recent-result eviction protection,
layout batching, budgets, and SourceMap copy metadata. The current-tree
evidence below was refreshed with the authoritative Rust 1.85 commands;
earlier C2b/C2c release logs remain historical context only.

The preceding D3 C2 release baseline was:

```text
C2b: durable_rows=51101 deltas=1000 layout_calls=0 history_bytes_cloned=0
C2c: p95_us=7610 p99_us=7932 durable_rows=43870 layout_calls=0
      history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=4396336
      retained_layout_bytes_estimate=8035080 c2c_max_tree_vm_hwm_kib=47172
Agent 0.5 serial E2E: 28/28
```

The C2c P95/P99 values are synthetic frame-processing samples for the fixed
workload; reruns can vary with host scheduling, and they are not terminal
input-to-frame latency.

The latest E1 implementation is `7ca349b` (start `740a528`; DTO/stream slice
`bd92e84`, detail slice `6653205`, capacity/harness hardening `e3f4af0`, then
visible-snapshot copy and tab-cache coherence). On **each** of Rust 1.85.0 and stable 1.97.1, `fmt --check`,
`test --locked --all-targets --no-fail-fast` (**762 passed, 0 failed, 39 ignored**),
Clippy and rustdoc with warnings denied passed. The real fixed-Agent serial run
passed **30/30 on each toolchain**, including all original 28 and the two new
Bash/detail/cancel workflows. All six ignored Release performance workloads
passed. The current C2c synthetic P95/P99 is **7941/8306 μs**; the block above
records the preceding D3 baseline, not a current terminal measurement.

Full commands, logs, scope, known unrun checks and the **338-file local/remote
tracked build-source equality manifest** are in
[`verification/v03-e1/README.md`](verification/v03-e1/README.md). E1 is ready for
parent review; this does not declare E2 or full-project v0.3 complete.

| ID | Required behavior | Status | Evidence / remaining work |
|---|---|---|---|
| REF-01 | Fixed Agent 0.5 / Protocol 1, no Agent/Runtime crate dependency | **Passed** | Protocol and pinned-Agent fixture/E2E coverage. |
| REF-02 | Protocol version and capability check; no 0.3 fallback | **Passed** | Bootstrap reducer and protocol tests. |
| REF-03 | Extended reasoning preserved without silent downgrade | **Passed** | Max-reasoning Agent E2E. |
| REF-04 | Response/event interleave, partial frame, and EOF behavior | **Passed** | RPC transport tests. |
| REF-05 | Full outbound admission does not block the UI; draft is retained | **Passed** | Backpressure and reducer tests. |
| REF-06 | Reserved control slots preserve FIFO safety | **Passed** | Backpressure baseline tests. |
| REF-07 | Read/deferred/byte budgets and expired-query accounting | **Passed** | Query slots, deferred cap, and wire-budget tests. |
| REF-08 | Local side effects stay off the input/RPC loop | **Passed** | Owned clipboard and job tests. |
| REF-09 | `session.read` reconstructs Runtime items across UTF-8 pages | **Passed** | `tests/read_chunks.rs` plus Agent fixtures; assembler now emits encoded items and production JSON decode is owned by `LocalJobs`. |
| REF-10 | Pinned prefix, cursor offsets, and new-pin rules | **Passed** | Read-chain pin/cursor tests. |
| REF-11 | Session-global and Turn-local indexes stay separate | **Passed** | History/result window tests. |
| REF-12 | Read-only browse does not open a Session or require Workspace | **Passed** | `Ctrl+B` browse issues `session.read` only; a real-Agent E2E browses a closed session after its workspace directory was deleted and its model's provider became unreachable (`e2e_browse_closed_session_survives_deleted_workspace_and_dead_model`), plus reducer tests for no-`session.open`, explicit continue and draft retention. |
| REF-13 | Large/missing/records-truncated/trailing-incomplete data is never fabricated | **Passed** | Bounded placeholder and malformed-page tests. |
| REF-14 | Deferred send has visible preparation and no automatic resend | **Passed** | B2 reducer and Agent preparation E2E. |
| REF-15 | Cancellation routes by exact operation ID or TurnRef | **Passed** | Operation ownership and cancellation tests. |
| REF-16 | Manual compact outcomes and unknown-write fence | **Passed** | Four-result fixtures, reducer tests, and Agent E2E. |
| REF-17 | Context estimate scope and separate utility usage | **Not run** | Stage B/E surface is not complete. |
| REF-18 | Config update applies to the next request without relabeling current Tool | **Passed** | Request-boundary and Agent E2E tests. |
| REF-19 | Steer accepted/applied/recorded are distinct and loop-scoped | **Passed** | Bounded steer queue and serial Agent E2E. |
| REF-20 | ACK cannot clear a newer editor revision | **Passed** | Editor-revision reducer tests. |
| REF-21 | Lost wait recovers through exact `turn.result` without rerunning tools | **Passed** | Recovery tests. |
| REF-22 | Failed save remains readable and Blocked/unknown facts are retained | **Passed** | Persistence-failure/result-retention tests. |
| REF-23 | Cancel does not claim rollback; close retains in-flight result | **Passed** | Lifecycle tests. |
| REF-24 | Reload does not stage or replace History/Live/draft state | **Passed** | Catalog-only reload tests and Agent E2E. |
| REF-25 | Per-session drafts, undo, paste, and cursor are independent | **Passed** | Each `SessionView` owns a whole `Composer` swapped on session change; reducer tests cover text/cursor/undo/paste markers plus the all-drafts admission budget (no silent truncation, one warning), and `e2e_session_switch_keeps_drafts_and_running_background_loop` proves independent drafts and a surviving background loop against the real Agent. |
| REF-26 | New/continue/rename/delete are explicit and project-safe | **Passed** | `/new` quick-creates in the current workspace with the catalog's recent explicit configuration (`/new form` and Ctrl+N keep the custom form); `--session <id>` is exact and `--continue` matches only the current workspace with a selector fallback; `/rename` uses the mutation-safe path; `/delete` is closed-only with an explicit confirm. Reducer tests plus `e2e_new_form_rename_close_delete_commands` and `e2e_startup_selection_opens_without_auto_prompt` (no auto-send, no cross-project guess) cover it. |
| REF-27 | One shared body owner and one ToolKey projection index | **Passed** | `tool_result_body_is_one_arc_across_live_history_and_presentation` proves ptr-equal body ownership across durable/live/presentation plus shared display ownership and weak-pointer release; ToolKey lookup remains indexed. |
| REF-28 | Live update has zero full-history clone and zero stable re-layout | **Passed** | C2b/C2c structural probes measured 1000 deltas, zero layout calls, zero historical text cloning, and viewport-only materialization. |
| REF-29 | Viewport/click/copy share layout; soft-wrap adds no copied newline | **Passed** | Shared immutable layout, SourceMap hard/soft-break tests, CJK/emoji/code-indent/blank-line/link tests, grapheme tests, and an unloaded-placeholder non-copy test. |
| REF-30 | Fold/resize/page-load preserve the scroll anchor | **Passed** | Five tests cover reasoning fold, width resize, earlier-history prepend, live→saved ToolID rebasing, and an unloaded-anchor nearest-retained fallback with a notice through `ScrollAnchor`. |
| REF-31 | Rail geometry and one-line Footer baseline remain stable | **Passed** | Rail fixtures and render snapshots. |
| REF-32 | Panel/focus cannot misfire send/cancel | **Passed** | E1 concrete MainView/Focus, F6/Esc/draft/scroll tests and detail-close real-Agent E2E; existing dock/search/confirmation regressions remain green. Future E2 pages must use the same contract. |
| REF-33 | Tool cards are not reordered or duplicated by completion | **Passed** | Tool completion and same-loop ordering tests. |
| REF-34 | `tool.read` distinguishes awaiting-policy/running/terminal | **Passed** | Pinned DTO fixtures plus E1 tool facts/recording/monotonic conflict tests; process command facts also survive arrival before read/execution. |
| REF-35 | base64/raw offset/UTF-8 tail/gap/EOF behavior | **Passed** | Twelve tool_streams tests (including tiny-chunk capacity/snapshot preservation) plus the real-Agent multi-page Unicode stdout and independent stderr drain. Late page EOF cannot hide newer event bytes. |
| REF-36 | stdout/stderr have no fake total order; stop is not confirmation | **Passed** | Separate stream tabs/cursors; nonzero exit 7 remains a command fact, and cancelling/termination_confirmed/output_complete are independent. Real exact-LoopRef Bash cancellation E2E passes. |
| REF-37 | Only visible tools read on demand; hidden/partial/expired states remain visible | **Passed** | E1 current-tab polling, close/tab generation fencing, fragmented-transport stale response accounting, 250/500 ms tests, retained-window labels, real EOF drain and explicit retry after query errors. |
| REF-38 | `@file` remains a path reference and preview does not attach content | **Not run** | Stage D/E. |
| REF-39 | workspace.files/search partial pages and cursor rules | **Not run** | D/E. |
| REF-40 | workspace.read fixed-revision paging and changed-data fencing | **Not run** | Stage E. |
| REF-41 | Changes workspace/tool origin and three comparisons | **Not run** | D/E. |
| REF-42 | Opaque change references and stale/fragment behavior | **Not run** | D/E. |
| REF-43 | Footer branches from explicit status and renderer performs no I/O | **Not run** | Stage E. |
| REF-44 | Search coverage distinguishes unloaded/large from global no-match | **Passed** | `/search` scans a loaded snapshot or an explicit pinned full-session `session.read` chain on the owned workers; coverage labels stay incomplete for large/stopped/failed/truncated scans. The real-Agent E2E scans 22 saved items across more than one page, finds a multi-byte UTF-8 literal, and reports complete coverage. |
| REF-45 | Prompt jump and temporary folds preserve selection | **Passed** | `/prev`, `/next`, `/latest` and match jumps skip steering, read an unloaded window at the exact index under the captured pin, and install temporary fold overrides that are restored when search closes (reducer tests). |
| REF-46 | Copy/export has no Rail/fake newline and export uses a fixed bounded pin | **Passed** | `/copy` reuses the rendered copy rows and hit operations with no remote read; `render_with_breaks` reports real logical line ends so a soft wrap never gains a newline and paragraph/code breaks survive. `/export` writes through one owned job from a pinned `session.read` chain with a bounded channel, unique temp file, explicit overwrite confirmation, atomic rename, cancel cleanup, explicit oversized placeholders, and an explicit raw-JSON path that verifies offsets/EOF before writing. The real-Agent E2E covers a multi-page UTF-8 history plus an in-progress live turn without mixing it into the saved pin. |
| REF-47 | External editor does not block RPC or overwrite a newer draft | **Passed** | Scripted direct-editor jobs cover atomic rename/readback, 0600 temp-file permissions, invalid UTF-8, 256 KiB rejection, nonzero exit, cancellation kill/wait/cleanup, and stale session/revision fencing; `tests/app_flow.rs` covers stale reducer return and the real Agent E2E keeps a background turn progressing while the editor runs. Real iTerm2 manual interaction remains explicitly **Not run**. |
| REF-48 | ANSI/OSC/control-safe display with raw protocol offsets preserved | **Passed** | Safe-display and control-sequence tests. |
| REF-49 | Logs contain no message/command/result/file/secret content | **Passed** | Content-free stderr/debug logging tests. |
| REF-50 | All cache/queue owners are bounded and background sessions release bodies | **Passed** | Budget tests cover viewport/neighbor/recent-result protection, farthest-first active-head eviction under the 32 MiB cap, 48 MiB layout eviction, background-first ordering, and weak-pointer release of history/layout owners; RSS remains an observation, not a mathematical proof. |
| REF-51 | Existing CJK/IME/mouse/scrollbar/terminal restore behavior | **Passed** | UI, Rail, terminal, and snapshot tests. |
| REF-52 | Common command table/completion/help stay consistent | **Passed** | `command::COMMANDS` is the single static table driving parsing, completion and the help panel; tests assert every table entry parses and is offered, unlisted names are unknown, and the help panel renders every entry. |
| REF-53 | No approval/plugin/Subagent/PTY/Git-write/auto-reconnect feature | **Passed** | Source audit. |
| REF-54 | Fixed-Agent E2E covers read/tool/compact/file/diff | **Not run** | 30/30 fixed-Agent E2Es pass on both toolchains, including E1 process streams/cancellation and all previous scenarios; workspace/file/changes/diff workflows are later E scope and remain uncovered. |
| REF-55 | Rust 1.85/stable and original tests on three platforms | **Not run** | Current E1 tree passes Rust 1.85.0 and stable 1.97.1 on remote Linux; macOS/Windows execution and terminal manual acceptance remain Not run. |
| REF-56 | Release before/after performance evidence uses real data | **Passed** | Current-tree Rust 1.85 focused C2b/C2c release probes passed; the full six-test ignored release suite also passed in this validation cycle. Workload units are recorded in `docs/performance.md`. |

## Counts

- **Passed**: 47
- **Failed**: 0
- **Not run**: 9
- **Not applicable**: 0

## C Status

- **C2b**: Passed on the recorded 50k-row / 1000-delta structural workload.
- **C2c mainline**: Passed on the recorded shared-owner/layout/budget/SourceMap
  workload and serial Agent E2E.
- **Full C release**: **Not run**. The bounded automatic history path is
  complete through 8 MiB, but a general typed/raw history-read workflow for
  oversized items and a fresh decode-throughput/RSS measurement are outside
  this boundary. D2's explicit `/export raw` path is separately covered by
  reducer fixtures; exact allocator/RSS accounting is not claimed.
- **D/E**: D1/D2/D3 and E1 tool details are implemented and validated (see
  `D Status` and the E1 evidence). Workspace/file/changes/context main-area
  pages remain later E scope; they are not implemented by this handoff.

## D Status

Stage D1 (per-session drafts, read-only browse, command surface) is complete
for the criteria below, on the tree validated by the logs above:

- **D1a per-session Composer**: **Passed.** Whole-composer swap on session
  change, scratch owner for no-session drafts, and a real admission budget:
  at 8 MiB retained across all drafts further typing/pasting is refused with
  one explicit warning and existing drafts are never truncated.
- **D1b read-only browse and startup selection**: **Passed.** Browse uses
  `session.read` alone after a closed session's workspace is deleted and its
  model is unusable; continuing is the explicit `Ctrl+G`/`/resume` action that
  opens without sending, and an open failure keeps the history and draft.
  `--session`/`--continue` never auto-prompt and never guess across projects.
- **D1c commands**: **Passed.** `/new` quick create with the current workspace
  and recent explicit configuration, `/new form` custom form, `/rename`
  mutation-safe rename, `/refresh` (view data) separate from `/reload`
  (configuration), `/clear` local reread, `/close` result reception and
  closed-only `/delete confirm`.
- **D1d command table**: **Passed.** One static table drives parse, help and
  completion; no unimplemented command is advertised.
- **D2 search/navigation**: **Passed.** `/search [full] [literal]` keeps the
  transcript visible, scans loaded content on the owned worker, and runs the
  explicit full-session scan as a pinned `session.read` chain whose items are
  decoded/scanned by the single decode worker. Coverage never claims a global
  no-match for large/stopped/failed/truncated scans; the real Agent E2E scans
  22 saved items over multiple pages and finds a UTF-8 literal. `n`/`p`/Enter
  jumps read an unloaded target window and temporary folds are restored on
  exit; Esc leaves the search first and never cancels a turn.
- **D2 copy**: **Passed.** `/copy [last|message|code|selection]` reuses the
  existing hit/copy operations, never issues a remote read, keeps the
  selection on a clipboard failure, and preserves real newlines without
  inventing one at a soft wrap (renderer-reported row breaks).
- **D2 export**: **Passed** for the criteria measured here, with the
  remaining environment limitations below. The reducer tests run the real
  writer (temp file beside the target, refusal until explicit `Ctrl+Y`,
  atomic rename, cancel cleanup, explicit placeholder plus `partial` note for
  an oversized item, raw chunk verification, and writer-slot ownership). The
  real-Agent E2E exports 22 saved items across multiple pages, preserves the
  UTF-8 body, keeps the in-progress turn outside the saved pin, and verifies
  the explicit unsaved inclusion is labeled `Unconfirmed live turn` with no
  temp file left.
- **D2 export limitations**: **Not run**. The fixed Agent's runtime model-text
  ceiling is 256 KiB, so a real-Agent >8 MiB oversized item cannot be produced;
  the >8 MiB raw-export path is validated with bounded reducer fixtures rather
  than claimed as a real-Agent measurement. Stable Linux passed for this
  current tree; macOS and Windows runs remain **Not run**.
- **D3 settings/editor/lifecycle**: **Passed** for the implemented slice.
  `config.rs` has strict TOML schema/duplicate-key handling, atomic persistence,
  CLI precedence, single-value editor environment parsing, and no provider
  secret/catalog fields. `/settings` is a real Dock with theme, thinking/tool
  defaults, editor executable/args, and Agent paths; path changes only warn
  that the next startup uses them. `/editor` uses one direct executable plus
  args and an OS-temp draft, suspends input/draw through `TerminalGuard`, keeps
  RPC/App/jobs alive, resumes and fences session/epoch/revision on return, and
  kills/waits/cleans up on shutdown. Startup failures retain executable,
  config-path, Agent-config, protocol, provider, and storage categories.
- **D1/D2/D3 parent review**: the parent/independent review of these commits
  has not been recorded here; "Passed" reflects the current tree's own measured
  evidence. E remains deferred until that review.

## Current C2c Follow-ups

1. Keep the passed Rust 1.85 fmt/tests/clippy, release probes, and 22-test
   Agent E2E evidence tied to the current workload units.
2. Keep oversized automatic history decoding explicit and bounded; no complete
   body may be fabricated from an 8 MiB placeholder path.
3. Leave the general oversized history-read workflow and
   decode-throughput/RSS measurement as **Not run**; do not convert the
   synthetic frame P95/P99 into terminal input-to-frame latency.
