# v0.3 Refactor Acceptance Matrix (REF-01…REF-56)

Status legend (only these four values are used; there is no "implemented"
or "partial" soft pass):

- **Passed** — the complete criterion is covered by named tests that were run
  and passed on the pinned toolchain. A criterion whose end-to-end half needs
  an ignored test can only pass if that test was actually executed and is
  recorded here.
- **Failed** — a required behavior is demonstrated absent or violated in the
  current tree.
- **Not run** — the acceptance check has not been fully executed (including
  partially implemented or partially measured behavior). The evidence cell
  states exactly what was measured and what is missing.
- **Not applicable** — the criterion does not apply to this project.

## Current evidence (commit `d47d837`)

Raw logs are on the builder at `/root/minicore-tui-v03-refactor/`. The
verification of the current tree is `c2b-fmt.log` / `c2b-tests.log` /
`c2b-clippy.log`; the real-Agent run is `c2b-agent-e2e.log`.

```bash
# remote /root/minicore-tui-v03-refactor/tui, RUSTUP_TOOLCHAIN=1.85.0
cargo fmt --all -- --check                       # clean
cargo test --locked --all-targets --no-fail-fast # 624 passed / 0 failed / 27 ignored
cargo clippy --locked --all-targets -- -D warnings  # clean
MINICORE_AGENT_BIN=... cargo test --locked --test agent_e2e -- --ignored --test-threads=1
# 22 passed / 0 failed / 0 ignored, log c2b-agent-e2e.log
```

The 27 ignored tests are the 22 real-Agent E2E scenarios plus 5 release/perf
tests. The E2E scenarios cover discovery, basic turn, max reasoning, tool
execution, steer pacing, update, shutdown cancel, reload, manual compaction
(`noop`, `compacted`, deferred cancel) and automatic preparation.

| ID | Required behavior | Status | Evidence / remaining |
|---|---|---|---|
| REF-01 | Fixed Agent 0.5 / Protocol 1, no Agent/Runtime crate dependency | **Passed** | B1 removed the `minor == 3` gate and validates `protocol_version == 1` plus the capability list (`src/app.rs::validate_backend`); reducer tests `bootstrap_accepts_the_pinned_agent_0_5_protocol_v1` / `bootstrap_rejects_a_backend_missing_required_capabilities`, and all 18 real-Agent E2E scenarios spawn the pinned 0.5.0 binary and reach `Ready`. |
| REF-02 | Protocol version + capability check; no 0.3 fallback | **Passed** | `agent.ping` parses `version`/`protocol_version`/`capabilities`; an incomplete capability list is rejected in the reducer test and every real-Agent E2E bootstrap exercises the accepted path. |
| REF-03 | Extended reasoning kept, no silent downgrade | **Passed** | `e2e_max_reasoning_ships_literal_max_and_provider_body_carries_it` drives a real turn and asserts the provider request body carries the literal `max` reasoning. |
| REF-04 | Response/Event interleave; partial frame and EOF | **Passed** | `src/rpc.rs` transport tests, including the 32 MiB frame bound and partial-EOF cases. |
| REF-05 | Send-full does not block UI; draft kept | **Passed** | `tests/backpressure_baseline.rs` drives a real spawned child that never reads stdin: 28 normal `try_send` calls succeed without awaiting, the 29th returns `QueueFull(Normal)`. `run_commands` revokes the pending registration and keeps the draft (`deferred_admission_ok`/`on_queue_full` tests). |
| REF-06 | Control reserve does not reorder queued same-session requests | **Passed** | Four reserved control slots stay usable after the normal class is full; the writer drains the single FIFO in admission order (`four_control_slots_stay_reserved_after_the_normal_class_is_full`). |
| REF-07 | read/deferred/byte budgets; expired query still counted | **Passed** | Two read-only in-flight slots with coalescing and a bounded waiting queue (`src/app/queries.rs` tests: `a_second_chain_for_one_view_coalesces_instead_of_taking_a_slot`, `only_two_distinct_reads_run_and_a_third_is_refused`, `invalidating_a_scope_drops_its_pending_refresh_but_keeps_the_slot`, `waiting_queue_is_bounded_and_fifo_fair`), `MAX_DEFERRED_REQUESTS = 16`, and the 64 MiB inbound wire budget released on ownership (`src/rpc.rs::consumed_frames_release_their_wire_charge`). |
| REF-08 | Async side effects off the input/RPC loop | **Passed** | `src/jobs.rs` owns the clipboard as a blocking task with owner/deadline/kill+wait; `run_commands` uses `try_send` and `AppCommand::CopySelection` only (`run_commands_admits_synchronously_and_owns_the_clipboard`). |
| REF-09 | session.read decodes real Runtime items, cross-page UTF-8 | **Passed** | `tests/read_chunks.rs` decodes the pinned Agent 0.5 fixtures: `paged_chunks_reconstruct_real_runtime_items_exactly`, `assembled_item_round_trips_the_canonical_bytes`, `continuation_offset_is_strict`, `complete_chunk_with_wrong_byte_count_is_rejected`, `unknown_encoding_is_refused`; `tests/agent_v1_fixtures.rs` decodes the recorded payloads. |
| REF-10 | Pinned prefix / non-zero cursor / new pin legal | **Passed** | `tests/read_chunks.rs::valid_snapshot_pins_require_the_agent_revision_shape` and the read-chain pin tests in `src/app/history.rs` (`continue_read_chain` revision/probe/cursor cases). |
| REF-11 | Session-global vs Turn-local index not mixed | **Passed** | `tests/read_chunks.rs::turn_result_window_keeps_turn_local_indexes_outside_session_history` and `turn_result_pages_decode_all_availabilities`; the history window keeps session-global indexes separately. |
| REF-12 | Read-only browse does not open Session/require Workspace | **Not run** | Stage E. |
| REF-13 | Large/missing/records_truncated/trailing_incomplete not faked complete | **Passed** | `tests/read_chunks.rs::oversized_item_becomes_a_placeholder`, `trailing_incomplete_tail_is_not_fabricated`, `records_truncated_is_reported`, `malformed_history_pages_do_not_advance_or_fabricate_completion`, `large_item_gap_is_rejected_before_placeholder_installation`; `tests/agent_v1_fixtures.rs::trailing_incomplete_history_is_reported_not_repaired`. |
| REF-14 | send may be deferred; preparation visible, no auto-resend | **Passed** | B2 preparing state plus deferred admission. `e2e_automatic_preparation_is_observable_and_cancellable` measures it against the real Agent: the second submit starts a real summary utility call, the app observes the live operation from `session.context`, Esc routes `session.compact.cancel` by that exact id, the deferred send fails and the prompt returns to the composer without a resend. |
| REF-15 | Cancel routes by exact operation ID or TurnRef | **Passed** | `RequestKind::CancelTurn(TurnRef)` / `CompactCancel { operation_id }`; reducer tests in `tests/app_flow.rs`, plus the real-Agent `e2e_manual_compact_deferred_cancel` which cancels a gated preparation by the locally observed operation id. The unknown-write fence never resends. |
| REF-16 | Manual compact four results + unknown_write | **Passed** | Real-Agent E2E measures `noop` (`e2e_manual_compact_without_history_is_a_noop`), `compacted` (`e2e_manual_compact_summarizes_history`) and cancel→`failed` (`e2e_manual_compact_deferred_cancel`); `tests/app_flow.rs::manual_compact_unknown_write_requires_fresh_state_and_context` covers the fourth outcome and the fence until state+context refresh. |
| REF-17 | Context estimate scope; utility usage separate | **Not run** | Stage B/E. |
| REF-18 | Update next Request; current Tool labels unchanged | **Passed** | `PendingConfigUpdate` evidence is settled by `RequestStarted`; `e2e_scenario_e_same_loop_update` and `e2e_scenario_e2_update_single_request_then_next_turn` pass against the real Agent. |
| REF-19 | Steer accepted/applied/recorded separated; no cross-Loop prompt | **Passed** | Bounded local steer queue (8 entries / 256 KiB) keyed to the exact `TurnRef`; `e2e_scenario_d_steer_turn`, `e2e_two_consecutive_steers_both_reach_the_provider` and `e2e_fifo_steers_are_paced_until_receipt` pass against the real Agent. |
| REF-20 | ACK does not clear a newer draft revision | **Passed** | `tests/app_flow.rs` steer/editor revision tests. |
| REF-21 | wait failure/lost event recovers via turn.result, no tool rerun | **Passed** | `tests/app_flow.rs::lost_wait_result_recovers_through_turn_result` reads `turn.result` by exact `TurnRef` and asserts no `turn.send`/tool rerun; `recover_turn` is the only recovery path. |
| REF-22 | Failed save retained result readable; Blocked/unknown correct | **Passed** | `tests/app_flow.rs::turn_result_completed_and_persistence_failed` plus the retained-result tests: a `persistence=failed` result stays readable, and Blocked/unknown handling follows the state/result evidence. |
| REF-23 | Cancel does not claim file rollback; close keeps in-flight result | **Passed** | `tests/app_flow.rs` lifecycle tests. |
| REF-24 | reload does not clear History/Live/draft or reinstall history | **Passed** | `fe59a49` narrowed reload to catalog generation only: the `ReloadState`/`ReloadPresentation`/`ReloadWaitTurn` requests, the staged state/presentation install and the `reload_fenced_*` patch branches are gone, and reload never touches Live, the draft, the selection or history. Reload-affected sessions are re-marked uncalibrated and converge through the normal read chain; `reload_does_not_stage_a_full_history_replacement` and the migrated reload tests plus the real-Agent `e2e_configuration_reload_refreshes_catalogs_only` measure it. |
| REF-25 | Per-session drafts/undo/paste/cursor independent | **Failed** | Draft state is one global `App.composer`; switching sessions does not keep an independent draft. D |
| REF-26 | New/continue/rename/delete explicit, no cross-project guess | **Not run** | Reducer tests exist; E2E 3 needs the real Agent. C/D |
| REF-27 | Single Arc body; single Tool index | **Not run** | The transcript projection now builds one `ToolKey` index per pass and resolves tools in O(1) (`stable_history_layout_is_cached_and_tool_projection_uses_the_index`), but the body is still duplicated between `RawHistoryItem` and the `TranscriptBlock` bridge, so the single `Arc<Message>` model is not built. |
| REF-28 | Live update zero full-history clone; zero stable re-layout | **Failed** | The new `PerfCounters` measure the real points: a cached durable layout is not rebuilt (`layout_calls` stops increasing), but every prepared frame still clones the durable rows (`historical_text_bytes_cloned > 0` in `stable_history_layout_is_cached_and_tool_projection_uses_the_index`), so the zero-clone half is not met. |
| REF-29 | viewport/click/copy share layout; softwrap adds no copy newline | **Not run** | C2. |
| REF-30 | Fold/resize/page-load keep anchor | **Not run** | C2. |
| REF-31 | Rail geometry / single-line Footer baseline | **Passed** | `src/ui/snapshots.rs`, `tests/rail_fixtures.rs`, `tests/render_snapshots.rs` pass unchanged; `logs_dark_80x24.txt` was regenerated for the content-free log rows. |
| REF-32 | Panel/focus does not misfire send/cancel | **Not run** | Stage E. |
| REF-33 | Tool cards not reordered by completion; no duplicate insert | **Passed** | `tests/app_flow.rs::tool_completion_updates_in_place_without_duplicate_or_reorder` + `deterministic_same_loop_model_a_to_tool_to_model_b`. |
| REF-34 | tool.read distinguishes awaiting_policy/running/terminal | **Not run** | Stage B/E; `tool.read` is not yet consumed by the TUI. |
| REF-35 | base64/raw offset/UTF-8 tail/gap/EOF correct | **Not run** | Stage B/E. |
| REF-36 | stdout/stderr no fake total order; stop request ≠ confirmed | **Not run** | Stage B/E. |
| REF-37 | Only visible tools read on demand; hidden stop; partial/expired visible | **Not run** | Stage E. |
| REF-38 | @file is a path reference; preview does not attach content | **Not run** | Stage D/E. |
| REF-39 | workspace.files/search partial + cursor rules | **Not run** | Stage B/E. |
| REF-40 | workspace.read same-revision in-line paging; changed not spliced | **Not run** | Stage E. |
| REF-41 | Changes workspace/tool origin + three comparisons | **Not run** | Stage B/E. |
| REF-42 | Opaque change_ref not parsed; stale/fragment correct | **Not run** | Stage B/E. |
| REF-43 | Footer branch from explicit status; renderer does no IO | **Not run** | Stage E. |
| REF-44 | Search coverage note; unloaded/large not "global no match" | **Not run** | Stage D. |
| REF-45 | Prompt jump; temporary fold does not break selection | **Not run** | Stage D. |
| REF-46 | Copy/export no Rail/fake newline; export fixed pin, bounded memory | **Not run** | Stage D. |
| REF-47 | External editor: background RPC continues; no draft overwrite | **Not run** | Stage D. |
| REF-48 | ANSI/OSC/control safe display; backend offset unchanged | **Passed** | `src/safe_text.rs::safe_display` is the single display boundary for markdown, plain wrap, filled rows, tool rows, selector rows, footer parts and error surfaces; `control_sequences_are_escaped_at_the_display_boundary` and the `safe_text` unit tests; escaping is display-only, protocol offsets still use the raw bytes. |
| REF-49 | Logs contain no message/command/result/file/secret | **Passed** | `RpcEvent::AgentStderr { bytes, dropped }` carries counts only; the app stores `agent stderr: N bytes`; `agent_stderr_is_never_stored_as_content` and the fatal-overlay/logs-panel tests. |
| REF-50 | All cache/queue bounded; background sessions release | **Failed** | Outbound 32 (28+4), wire 64 MiB, 2 read slots, 16 deferred, the Composer 256 KiB cap and the job deadline are bounded and measured. `HistoryWindow` still tracks but does not enforce a 32 MiB body budget and never evicts, and the Composer 8 MiB all-drafts budget has no owner yet, so this criterion fails. |
| REF-51 | Existing CJK/IME/mouse/scrollbar/Terminal restore preserved | **Passed** | `ui::*`, `tests/terminal_restore.rs`, `tests/rail_fixtures.rs`. |
| REF-52 | Common command table/completion/help consistent | **Failed** | Three separate lists still diverge; `/refresh` missing. D |
| REF-53 | No approval/plugin/Subagent/PTY/Git-write/auto-reconnect | **Passed** | Source audit: no such code. |
| REF-54 | Fixed-Agent E2E covers read/tool/compact/file/diff | **Failed** | 22 real-Agent scenarios exist and all pass (`c1l-agent-e2e.log`), covering read, tool, steer, update, shutdown, reload, manual compaction (`noop`, `compacted`, deferred cancel) and automatic preparation (observable + cancellable). Only workspace file/changes/diff scenarios are still missing, so this criterion cannot pass yet. |
| REF-55 | Rust 1.85/stable, three-platform original tests pass | **Not run** | Full suite, fmt and clippy run clean under `RUSTUP_TOOLCHAIN=1.85.0` on remote Linux. macOS/Windows have not been run in this refactor; the row stays Not run until they are. |
| REF-56 | Release perf before/after with real data, not faked | **Not run** | Before data in `docs/performance.md`; after is C2/F. |

## Status counts

- **Passed** (28): REF-01, 02, 03, 04, 05, 06, 07, 08, 09, 10, 11, 13, 14,
  15, 16, 18, 19, 20, 21, 22, 23, 24, 31, 33, 48, 49, 51, 53.
- **Failed** (5): REF-25, 28, 50, 52, 54.
- **Not run** (23): REF-12, 17, 26, 27, 29, 30, 32, 34, 35, 36, 37, 38, 39,
  40, 41, 42, 43, 44, 45, 46, 47, 55, 56.
- **Not applicable** (0).

## C1 progress and remaining work

Landed and verified (each commit was tested remotely on Rust 1.85):

1. `c843105` — synchronous bounded admission (32 = 28+4), 1 MiB line bound,
   FIFO writer, 64 MiB inbound wire budget released on ownership, deferred
   retries keyed by exact target, content-free stderr, unified safe-display
   boundary.
2. `d47d837` — `SessionView.history_read: HistoryRead { active, pending }` with
   `HistoryTrigger::{Refresh, Gap, PostWait}` replaces `loading` /
   `reconcile_inflight` / `needs_post_wait_history`; the header, takeover and
   wait reducers read the converged state.
3. `3d159a7` — `LocalJobs` owns exactly one clipboard job; a second copy is
   refused without queueing; the debug log writes from a dedicated thread and
   never blocks the UI; refused sends have bounded retries (initial + ≤2) and
   abandoned input is restored to the composer or returned to the paused steer
   queue, never dropped.
4. `f440741` + `b3eeda8` — `result_unconfirmed: bool` becomes
   `ResultConfirmation::{Confirmed, NeedsRead, Unknown}` with per-site evidence
   and recovery, and a known failed save no longer leaks into the forced
   shutdown message.
5. `fe59a49` — reload narrows to configuration→models/profiles/metadata
   catalog generation; `ReloadState` / `ReloadPresentation` /
   `ReloadWaitTurn`, the staged install and the `reload_fenced_*` branches are
   removed and the ~26 affected reload tests migrated to conservative
   re-read semantics. (This commit's first push had fmt/clippy drift, fixed in
   the next commit.)
6. `77c0ebf` — session catalogs are generation-based: a late
   `session.list` that predates a rename/delete can no longer resurrect a
   title override or a deleted row; stale bootstrap/refresh/reload lists are
   re-issued with their own generation.
7. `c91a686` — three real-Agent manual-compaction E2E scenarios (`noop`,
   `compacted`, gated deferred cancel).
8. The read/result chain (20 methods, ~1.1k lines: the `session.read`
   pin/assembler/paging chain, gap reconciliation and `turn.result`
   recovery) moved out of `app.rs` into `pub(super)` methods in
   `app/history.rs`.
9. The session lifecycle group (45 methods, ~2.1k lines:
   create/open/close/delete/rename, the state/presentation/context reads and
   responses, and the catalog generation that owns `session.list`) moved into
   `pub(super)` methods in `app/session.rs`.
10. The turn state machine (47 methods, ~2.4k lines: submission, the bounded
   steer queue, deferred `turn.wait`/`turn.result` slots, cancellation,
   manual compaction and the local clipboard/stderr jobs) moved into
   `pub(super)` methods in `app/turn.rs`; the source-scanning baseline tests
   now read all app module files as one source so they keep checking the same
   invariants.
11. The query-slot/context-poll group (`read_context_command`,
   `free_query_slot`, `drain_query_followups`, `context_interval`,
   `context_query_pending`, `arm_context_poll`, `poll_contexts`,
   `reschedule_context_poll`) moved into `pub(super)` methods in
   `app/queries.rs`, next to the `QuerySlots`/`QueryKey` types. `app.rs` is
   10.0k lines; all four named modules exist.
12. This commit — `e2e_automatic_preparation_is_observable_and_cancellable`
   closes the last real-Agent gap: a large first answer pushes the estimated
   history over the automatic trigger, the second submit runs a gated summary
   utility call, the app observes the operation from `session.context` and
   Esc cancels it by the exact id; the prompt returns to the composer.

Verified evidence at `c91a686`: `fmt` clean, `620 passed / 0 failed / 26
ignored`, `clippy -D warnings` clean, tree md5
`ee2180393a5993ba47daed879aa31328`; real-Agent E2E `21 passed / 0 failed`
(`c1f-agent-e2e.log`). Logs: `c1a-*`..`c1g-*` under
`/root/minicore-tui-v03-refactor/`.

Still open in C1 (do not claim C1 complete):

- The C2/C3 items behind REF-25/28/52/54 (per-session draft state, shared
  history body/layout, file/changes/diff E2E scenarios) are untouched.
  The four named app modules exist and `app.rs` shrank from 15.8k to 10.0k
  lines; the remainder is the `App` owner/fields, `update` and the event
  router, navigation/selector logic and the small clocks by design, so a
  further split is optional follow-up, not a C1 blocker.

## C2 progress (incomplete)

C2 is not complete; this is the measured state of the first slice.

Landed and verified in `835c27c`:

- `src/perf.rs` thread-local `PerfCounters` incremented at the real execution
  points (layout rebuilds, durable bytes cloned, owned rows materialized,
  Composer full joins, tool index lookups/linear scans, retained history
  bytes). Tests read the counters; no constant-zero evidence is used.
- The transcript projection resolves tool calls through one `ToolKey` index
  per pass; the per-call block scan is gone and `tool_linear_scans` is
  asserted zero.
- `Composer` keeps a cached `byte_len`; ordinary typing after a 256 KiB paste
  performs no full-buffer join (`composer_full_joins` unchanged).
- `prepare_conversation` counts a durable layout rebuild only on a cache miss.

Measured structure at this commit: a cached durable layout is not rebuilt, but
each prepared frame still clones all durable rows (`historical_text_bytes_cloned`
is non-zero). The C2 completion threshold therefore is **not** met; the
remaining work is:

- `ConversationLayout` sections with integer prefix offsets and a
  viewport+overscan `VisibleConversation` shared by draw/hit/copy/scrollbar
  (spec §11.2/§11.7).
- `HistoryWindow` 32 MiB body budget with eviction of confirmed content from
  inactive, far-from-viewport sessions (spec §6.5/§21).
- Composer 8 MiB all-drafts budget with a real per-session draft owner
  (spec §12.1/§21) — this is also REF-25's prerequisite.
- 48 MiB layout cache enforcement and the single bounded layout worker
  (spec §11.5/§11.6).
- Release perf run at 120x40 with P95/P99 and RSS (spec §25.2) — Not run.

## C1 review fixes (this commit)

The first C2 session closed the C1 review items:

- **Status vocabulary** — the matrix now uses only Passed / Failed / Not run /
  Not applicable. Every row that previously said "Implemented" or "Partial"
  carries a measured status plus the exact evidence or missing check; no row
  is a soft pass.
- **Admission semantics** — `on_queue_full` no longer auto-retries ordinary
  sends. `turn.send`/`turn.steer`/`session.update` restore the user's input
  once and report Busy so the user decides whether to submit again. Only
  never-written control intents (`turn.cancel`, `session.compact.cancel`) and
  never-written settlement reads (`turn.wait`, `turn.result`) are retained by
  exact target and re-emitted when the FIFO admits them; a cancel is never
  abandoned because the queue stayed full. A request that was already written
  and then failed is never replayed. Tests:
  `a_queue_full_send_restores_the_prompt_and_never_auto_retries`,
  `a_refused_cancel_is_retained_until_it_is_admitted`,
  `a_queue_full_steer_returns_to_the_paused_queue`.
- **Deletion is catalog generation** — `SessionsState.deleted` (the permanent
  tombstone set) is gone. A delete removes the view and the list row and bumps
  the catalog generation; stale `session.list` responses are dropped by
  generation, `upsert_session_list` only applies to a session that still has a
  view, and every late-response guard uses `App::session_absent` (no view, no
  list row). `title_overrides` was already removed by `77c0ebf`.
- **Clipboard reclamation risk** — killing the direct child does not
  necessarily close a pipe inherited by a spawned descendant. The writer is
  therefore detached instead of joined on the deadline so the caller stays
  bounded, and the residual leak is documented in `src/clipboard.rs` and
  `src/jobs.rs` instead of claiming a hung helper can never block.
