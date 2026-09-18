# v0.3 Refactor Acceptance Matrix (REF-01…REF-56)

Status legend:

- **Passed** — the complete behavior is implemented and measured by tests that
  run in the default suite on the pinned toolchain.
- **Implemented** — the behavior is implemented and measured by unit/reducer
  tests, but the fixed-Agent E2E scenario that would prove it end-to-end is
  still `#[ignore]`d on this host (it needs `MINICORE_AGENT_BIN` and the
  loopback mock). Not a pass yet.
- **Partial** — one named part of the criterion is implemented and measured;
  the remainder is named in the evidence cell.
- **Not run** — neither implemented nor measured yet; the stage that will
  cover it is named. Wire fixtures pinned at stage A are contract evidence,
  never a pass for production behavior.
- **Failed** — a demonstrated violation still exists in the current tree.

## Current evidence (commit `d47d837`)

Raw logs are on the builder at `/root/minicore-tui-v03-refactor/`. The
verification of this exact tree is `c1-final-tests4.log`; the per-slice logs
are `c1-io-tests4.log`, `c1-history-tests.log` and `c1-io-clippy.log`.

```bash
# remote /root/minicore-tui-v03-refactor/tui, RUSTUP_TOOLCHAIN=1.85.0
cargo fmt --all -- --check                       # clean
cargo test --locked --all-targets --no-fail-fast # 610 passed / 0 failed / 23 ignored
cargo clippy --locked --all-targets -- -D warnings  # clean
```

The 23 ignored tests are the 18 real-Agent E2E scenarios plus 5 release/perf
tests. The E2E suite **was run** for this commit against the pinned binary:

```bash
MINICORE_AGENT_BIN=/root/minicore-tui-v03-refactor/agent-target/debug/minicore-agent \
  cargo test --locked --test agent_e2e -- --ignored --test-threads=1
# 18 passed / 0 failed / 0 ignored, log c1-agent-e2e.log
```

Those 18 scenarios cover discovery, basic turn, max reasoning, tool execution,
steer pacing, update, shutdown cancel, reload and stress; they do **not** cover
manual compaction or automatic preparation, so those two behaviors stay below
their implementation status.

| ID | Required behavior | Status | Evidence / remaining |
|---|---|---|---|
| REF-01 | Fixed Agent 0.5 / Protocol 1, no Agent/Runtime crate dependency | **Passed** | B1 removed the `minor == 3` gate and validates `protocol_version == 1` plus the capability list (`src/app.rs::validate_backend`); reducer tests `bootstrap_accepts_the_pinned_agent_0_5_protocol_v1` / `bootstrap_rejects_a_backend_missing_required_capabilities`, and all 18 real-Agent E2E scenarios spawn the pinned 0.5.0 binary and reach `Ready`. |
| REF-02 | Protocol version + capability check; no 0.3 fallback | **Passed** | `agent.ping` parses `version`/`protocol_version`/`capabilities`; an incomplete capability list is rejected in the reducer test and every real-Agent E2E bootstrap exercises the accepted path. |
| REF-03 | Extended reasoning kept, no silent downgrade | **Passed** | `e2e_max_reasoning_ships_literal_max_and_provider_body_carries_it` drives a real turn and asserts the provider request body carries the literal `max` reasoning. |
| REF-04 | Response/Event interleave; partial frame and EOF | **Passed** | `src/rpc.rs` transport tests, including the 32 MiB frame bound and partial-EOF cases. |
| REF-05 | Send-full does not block UI; draft kept | **Passed** | `tests/backpressure_baseline.rs` drives a real spawned child that never reads stdin: 28 normal `try_send` calls succeed without awaiting, the 29th returns `QueueFull(Normal)`. `run_commands` revokes the pending registration and keeps the draft (`deferred_admission_ok`/`on_queue_full` tests). |
| REF-06 | Control reserve does not reorder queued same-session requests | **Passed** | Four reserved control slots stay usable after the normal class is full; the writer drains the single FIFO in admission order (`four_control_slots_stay_reserved_after_the_normal_class_is_full`). |
| REF-07 | read/deferred/byte budgets; expired query still counted | **Implemented** | Two read-only slots plus `MAX_WAITING = 16` in `src/app/queries.rs`; `MAX_DEFERRED_REQUESTS = 16` and coalesced retry intents in `src/app.rs`; 64 MiB inbound wire budget released when the app owns a frame (`consumed_frames_release_their_wire_charge`). Disconnect/expiry accounting covered by unit tests. |
| REF-08 | Async side effects off the input/RPC loop | **Passed** | `src/jobs.rs` owns the clipboard as a blocking task with owner/deadline/kill+wait; `run_commands` uses `try_send` and `AppCommand::CopySelection` only (`run_commands_admits_synchronously_and_owns_the_clipboard`). |
| REF-09 | session.read decodes real Runtime items, cross-page UTF-8 | **Implemented** | `src/protocol/read.rs` decodes `utf8_json` envelopes per page; the pagination tests cover split UTF-8 and no-progress cursors. Real-Agent E2E still ignored. |
| REF-10 | Pinned prefix / non-zero cursor / new pin legal | **Implemented** | Read chain carries `pin`/`cursor`/`window_start`; probe-without-pin establishes the prefix (`src/app.rs::continue_read_chain`, pin tests in `src/app.rs` tests). |
| REF-11 | Session-global vs Turn-local index not mixed | **Implemented** | `turn.result` items decode into a turn-local window; history window is keyed by the session-global index (`src/protocol/read.rs`, `tests/protocol.rs`). |
| REF-12 | Read-only browse does not open Session/require Workspace | **Not run** | Stage E. |
| REF-13 | Large/missing/records_truncated/trailing_incomplete not faked complete | **Implemented** | Placeholders keep unloaded ranges explicit; `trailing_incomplete` is decoded as incomplete and never repaired. |
| REF-14 | send may be deferred; preparation visible, no auto-resend | **Implemented** | B2 preparing state plus deferred admission; `prepare` cancel routes to `session.compact.cancel` by operation id. Real-Agent prep E2E scenario not yet added. |
| REF-15 | Cancel routes by exact operation ID or TurnRef | **Implemented** | `RequestKind::CancelTurn(TurnRef)` / `CompactCancel { operation_id }`; unknown-write fence never resends. `e2e_manual_compact_deferred_cancel` measures the deferred `session.compact.cancel` path against the real Agent by the locally known operation id. |
| REF-16 | Manual compact four results + unknown_write | **Implemented** | Four `CompactStatusWire` outcomes decoded. Real-Agent E2E now measures `noop` (`e2e_manual_compact_without_history_is_a_noop`), `compacted` (`e2e_manual_compact_summarizes_history`) and cancel→`failed` (`e2e_manual_compact_deferred_cancel`); `unknown_write` keeps the fence until state+context refresh and is only reducer-tested. |
| REF-17 | Context estimate scope; utility usage separate | **Not run** | Stage B/E. |
| REF-18 | Update next Request; current Tool labels unchanged | **Passed** | `PendingConfigUpdate` evidence is settled by `RequestStarted`; `e2e_scenario_e_same_loop_update` and `e2e_scenario_e2_update_single_request_then_next_turn` pass against the real Agent. |
| REF-19 | Steer accepted/applied/recorded separated; no cross-Loop prompt | **Passed** | Bounded local steer queue (8 entries / 256 KiB) keyed to the exact `TurnRef`; `e2e_scenario_d_steer_turn`, `e2e_two_consecutive_steers_both_reach_the_provider` and `e2e_fifo_steers_are_paced_until_receipt` pass against the real Agent. |
| REF-20 | ACK does not clear a newer draft revision | **Passed** | `tests/app_flow.rs` steer/editor revision tests. |
| REF-21 | wait failure/lost event recovers via turn.result, no tool rerun | **Implemented** | `recover_turn` reads `turn.result` by exact `TurnRef`; `lost_wait_result_recovers_through_turn_result`. |
| REF-22 | Failed save retained result readable; Blocked/unknown correct | **Implemented** | Retained result is readable after `pending → stored`; `agent_exit_marks_live_result_unconfirmed_without_overwriting_known_result`. |
| REF-23 | Cancel does not claim file rollback; close keeps in-flight result | **Passed** | `tests/app_flow.rs` lifecycle tests. |
| REF-24 | reload does not clear History/Live/draft or reinstall history | **Passed** | `fe59a49` narrowed reload to catalog generation only: the `ReloadState`/`ReloadPresentation`/`ReloadWaitTurn` requests, the staged state/presentation install and the `reload_fenced_*` patch branches are gone, and reload never touches Live, the draft, the selection or history. Reload-affected sessions are re-marked uncalibrated and converge through the normal read chain; `reload_does_not_stage_a_full_history_replacement` and the migrated reload tests plus the real-Agent `e2e_configuration_reload_refreshes_catalogs_only` measure it. |
| REF-25 | Per-session drafts/undo/paste/cursor independent | **Failed** | Draft state is one global `App.composer`; switching sessions does not keep an independent draft. D |
| REF-26 | New/continue/rename/delete explicit, no cross-project guess | **Not run** | Reducer tests exist; E2E 3 needs the real Agent. C/D |
| REF-27 | Single Arc body; single Tool index | **Not run** | C2. Reload no longer duplicates history bodies, but the shared body/index is not built. |
| REF-28 | Live update zero full-history clone; zero stable re-layout | **Failed** | `tests/performance.rs` shows full-history materialization; counters are C2. |
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
| REF-50 | All cache/queue bounded; background sessions release | **Partial** | Outbound 32 (28+4), wire 64 MiB, 2 read slots, 16 deferred, jobs deadline: all bounded and measured. The global 32 MiB transcript cache with per-session eviction is C2. |
| REF-51 | Existing CJK/IME/mouse/scrollbar/Terminal restore preserved | **Passed** | `ui::*`, `tests/terminal_restore.rs`, `tests/rail_fixtures.rs`. |
| REF-52 | Common command table/completion/help consistent | **Failed** | Three separate lists still diverge; `/refresh` missing. D |
| REF-53 | No approval/plugin/Subagent/PTY/Git-write/auto-reconnect | **Passed** | Source audit: no such code. |
| REF-54 | Fixed-Agent E2E covers read/tool/compact/file/diff | **Failed** | 21 real-Agent scenarios exist and all pass (`c1f-agent-e2e.log`), covering read, tool, steer, update, shutdown, reload and manual compaction (`noop`, `compacted`, deferred cancel). Automatic preparation and workspace file/changes/diff scenarios are still missing, so this criterion cannot pass yet. |
| REF-55 | Rust 1.85/stable, three-platform original tests pass | **Partial** | Full suite, fmt and clippy run clean under `RUSTUP_TOOLCHAIN=1.85.0` on remote Linux. macOS/Windows not run. |
| REF-56 | Release perf before/after with real data, not faked | **Not run** | Before data in `docs/performance.md`; after is C2/F. |

## Status counts

- **Passed** (18): REF-01, 02, 03, 04, 05, 06, 08, 18, 19, 20, 23, 24, 31,
  33, 48, 49, 51, 53.
- **Implemented** (10): REF-07, 09, 10, 11, 13, 14, 15, 16, 21, 22.
- **Partial** (2): REF-50, 55.
- **Not run** (22): REF-12, 17, 26, 27, 29, 30, 32, 34, 35, 36, 37, 38, 39,
  40, 41, 42, 43, 44, 45, 46, 47, 56, plus the real-Agent E2E half of every
  **Implemented** row.
- **Failed** (4): REF-25, 28, 52, 54.

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
8. This commit — the read/result chain (20 methods, ~1.1k lines: the
   `session.read` pin/assembler/paging chain, gap reconciliation and
   `turn.result` recovery) moved out of `app.rs` into `pub(super)` methods in
   `app/history.rs`; the earlier `app/ui_actions.rs` group is unchanged.

Verified evidence at `c91a686`: `fmt` clean, `620 passed / 0 failed / 26
ignored`, `clippy -D warnings` clean, tree md5
`ee2180393a5993ba47daed879aa31328`; real-Agent E2E `21 passed / 0 failed`
(`c1f-agent-e2e.log`). Logs: `c1a-*`..`c1g-*` under
`/root/minicore-tui-v03-refactor/`.

Still open in C1 (do not claim C1 complete):

- `src/app.rs` (14.7k lines) is only partially split: the read/result chain
  now lives in `app/history.rs` as real methods, while `app/session.rs`,
  `app/turn.rs` and the remaining `app/queries.rs` groups are not split out
  yet and app.rs still owns the session/turn/UI reducers.
- No real-Agent automatic-preparation E2E scenario has been added; the 21
  scenarios cover manual compaction but not `session.context.current_operation`
  preparation-on-submit.
- The C2/C3 items behind REF-25/28/52/54 (per-session draft state, shared
  history body/layout, file/diff E2E) are untouched.
