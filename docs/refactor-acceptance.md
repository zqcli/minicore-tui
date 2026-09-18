# v0.3 Refactor Acceptance Matrix (REF-01…REF-56)

Status values are deliberately strict:

- **Passed** — the complete criterion has named coverage and the recorded
  validation passed.
- **Failed** — the current tree demonstrably violates the criterion.
- **Notrun** — the criterion is missing, only partially covered, or its final
  validation has not been run on the current tree.
- **Not applicable** — the criterion does not apply to this project.

## Current Evidence

The authoritative builder is `root@192.168.20.199`, workspace
`/root/minicore-tui-v03-refactor/tui`, with Rust 1.85.0. The current C2c
mainline includes the bounded serialized canonical-item decode worker, shared
history owners, ToolFacts presentation ownership, layout batching, budgets,
and SourceMap copy metadata. The current-tree evidence below was refreshed
with the authoritative Rust 1.85 commands; earlier C2b/C2c release logs remain
historical context only.

The current-tree C2 release evidence is:

```text
C2b: durable_rows=51101 deltas=1000 layout_calls=0 history_bytes_cloned=0
C2c: p95_us=2876 p99_us=3154 durable_rows=43870 layout_calls=0
      history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=4396336
      retained_layout_bytes_estimate=8035080 c2c_max_tree_vm_hwm_kib=47172
Agent 0.5 serial E2E: 22/22
```

The C2c P95/P99 values are synthetic frame-processing samples for the fixed
workload, not terminal input-to-frame latency.

Current-tree validation also passed Rust 1.85.0 `fmt --check`,
`test --locked --all-targets` (446 library tests and all integration suites
with zero failures), and Clippy with `-D warnings`. The real Agent 0.5 serial
run passed 22/22 tests.

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
| REF-12 | Read-only browse does not open a Session or require Workspace | **Notrun** | Stage E. |
| REF-13 | Large/missing/records-truncated/trailing-incomplete data is never fabricated | **Passed** | Bounded placeholder and malformed-page tests. |
| REF-14 | Deferred send has visible preparation and no automatic resend | **Passed** | B2 reducer and Agent preparation E2E. |
| REF-15 | Cancellation routes by exact operation ID or TurnRef | **Passed** | Operation ownership and cancellation tests. |
| REF-16 | Manual compact outcomes and unknown-write fence | **Passed** | Four-result fixtures, reducer tests, and Agent E2E. |
| REF-17 | Context estimate scope and separate utility usage | **Notrun** | Stage B/E surface is not complete. |
| REF-18 | Config update applies to the next request without relabeling current Tool | **Passed** | Request-boundary and Agent E2E tests. |
| REF-19 | Steer accepted/applied/recorded are distinct and loop-scoped | **Passed** | Bounded steer queue and serial Agent E2E. |
| REF-20 | ACK cannot clear a newer editor revision | **Passed** | Editor-revision reducer tests. |
| REF-21 | Lost wait recovers through exact `turn.result` without rerunning tools | **Passed** | Recovery tests. |
| REF-22 | Failed save remains readable and Blocked/unknown facts are retained | **Passed** | Persistence-failure/result-retention tests. |
| REF-23 | Cancel does not claim rollback; close retains in-flight result | **Passed** | Lifecycle tests. |
| REF-24 | Reload does not stage or replace History/Live/draft state | **Passed** | Catalog-only reload tests and Agent E2E. |
| REF-25 | Per-session drafts, undo, paste, and cursor are independent | **Notrun** | Per-session draft workflow is D scope and has not been covered. |
| REF-26 | New/continue/rename/delete are explicit and project-safe | **Notrun** | Reducer coverage exists; final workflow coverage is not complete. |
| REF-27 | One shared body owner and one ToolKey projection index | **Notrun** | ToolFacts and Arc result sharing are landed, but final single-Arc body deduplication across all durable/live/presentation paths is not complete. |
| REF-28 | Live update has zero full-history clone and zero stable re-layout | **Passed** | C2b/C2c structural probes measured 1000 deltas, zero layout calls, zero historical text cloning, and viewport-only materialization. |
| REF-29 | Viewport/click/copy share layout; soft-wrap adds no copied newline | **Passed** | Shared immutable layout, SourceMap hard/soft-break tests, link/grapheme tests, and existing copy tests. |
| REF-30 | Fold/resize/page-load preserve the scroll anchor | **Notrun** | Current geometry tests do not cover the complete cross-boundary acceptance workflow. |
| REF-31 | Rail geometry and one-line Footer baseline remain stable | **Passed** | Rail fixtures and render snapshots. |
| REF-32 | Panel/focus cannot misfire send/cancel | **Notrun** | Stage E. |
| REF-33 | Tool cards are not reordered or duplicated by completion | **Passed** | Tool completion and same-loop ordering tests. |
| REF-34 | `tool.read` distinguishes awaiting-policy/running/terminal | **Notrun** | Backend DTOs exist; the TUI does not consume the workflow. |
| REF-35 | base64/raw offset/UTF-8 tail/gap/EOF behavior | **Notrun** | D/E tool-read workflow. |
| REF-36 | stdout/stderr have no fake total order; stop is not confirmation | **Notrun** | Full workflow coverage is not complete. |
| REF-37 | Only visible tools read on demand; hidden/partial/expired states remain visible | **Notrun** | Stage E. |
| REF-38 | `@file` remains a path reference and preview does not attach content | **Notrun** | Stage D/E. |
| REF-39 | workspace.files/search partial pages and cursor rules | **Notrun** | D/E. |
| REF-40 | workspace.read fixed-revision paging and changed-data fencing | **Notrun** | Stage E. |
| REF-41 | Changes workspace/tool origin and three comparisons | **Notrun** | D/E. |
| REF-42 | Opaque change references and stale/fragment behavior | **Notrun** | D/E. |
| REF-43 | Footer branches from explicit status and renderer performs no I/O | **Notrun** | Stage E. |
| REF-44 | Search coverage distinguishes unloaded/large from global no-match | **Notrun** | Stage D. |
| REF-45 | Prompt jump and temporary folds preserve selection | **Notrun** | Stage D. |
| REF-46 | Copy/export has no Rail/fake newline and export uses a fixed bounded pin | **Notrun** | Stage D. |
| REF-47 | External editor does not block RPC or overwrite a newer draft | **Notrun** | Stage D. |
| REF-48 | ANSI/OSC/control-safe display with raw protocol offsets preserved | **Passed** | Safe-display and control-sequence tests. |
| REF-49 | Logs contain no message/command/result/file/secret content | **Passed** | Content-free stderr/debug logging tests. |
| REF-50 | All cache/queue owners are bounded and background sessions release bodies | **Notrun** | Enforcement exists for History/Layout/Live/Tool/Composer and decode queues, but exact capacity/RSS accounting and final release workload must still be rerun and reviewed. |
| REF-51 | Existing CJK/IME/mouse/scrollbar/terminal restore behavior | **Passed** | UI, Rail, terminal, and snapshot tests. |
| REF-52 | Common command table/completion/help stay consistent | **Failed** | The command surface still has known divergence; D scope. |
| REF-53 | No approval/plugin/Subagent/PTY/Git-write/auto-reconnect feature | **Passed** | Source audit. |
| REF-54 | Fixed-Agent E2E covers read/tool/compact/file/diff | **Notrun** | 22/22 current Agent E2E covers read/tool/compact and lifecycle paths; workspace file/changes/diff workflows are D/E scope and not covered. |
| REF-55 | Rust 1.85/stable and original tests on three platforms | **Notrun** | Rust 1.85 remote Linux is authoritative; current-tree stable/macOS/Windows coverage is not complete. |
| REF-56 | Release before/after performance evidence uses real data | **Passed** | Current-tree Rust 1.85 focused C2b/C2c release probes passed; the full six-test ignored release suite also passed in this validation cycle. Workload units are recorded in `docs/performance.md`. |

## Counts

- **Passed**: 33
- **Failed**: 1
- **Notrun**: 22
- **Not applicable**: 0

## C Status

- **C2b**: Passed on the recorded 50k-row / 1000-delta structural workload.
- **C2c mainline**: Passed on the recorded shared-owner/layout/budget/SourceMap
  workload and serial Agent E2E.
- **Full C release**: **Notrun**. A bounded worker now decodes complete items
  up to the 8 MiB automatic ceiling, but typed explicit decoding for items over
  8 MiB, final single-Arc body deduplication across every Tool path, exact
  capacity/RSS accounting remain.
- **D/E**: Search/export/workspace workflows are intentionally not started and
  must not be reported as C defects.

## Current C2c Follow-ups

1. Keep the passed Rust 1.85 fmt/tests/clippy, release probes, and 22-test
   Agent E2E evidence tied to the current workload units.
2. Keep oversized automatic history decoding explicit and bounded; no complete
   body may be fabricated from an 8 MiB placeholder path.
3. Finish the final shared tool-body owner and release/weak-pointer evidence
   before claiming full C.
