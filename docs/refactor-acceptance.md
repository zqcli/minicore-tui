# v0.3 Refactor Acceptance Matrix (REF-01…REF-56)

Status legend: **Passed** / **Failed** / **Not run** / **Not applicable**.

Rules applied here, after stage-A review:

- **Passed** requires the *complete* behavior to be implemented **and** measured
  on this baseline. Pinning a wire sample is **not** a pass for a behavior the
  production TUI must implement.
- **Failed** means the old (v0.2.8) code has a known, demonstrated violation of
  the criterion. A RED baseline test that passes *because the defect exists*
  documents a **Failed** criterion, not a pass.
- **Not run** means the behavior is neither implemented nor yet fully
  contradicted; the stage that will cover it is named. Known failures are not
  hidden under this label.

Evidence commands (remote `/root/minicore-tui-v03-refactor/tui`):

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/generate_agent_v1_fixtures.py --agent-bin <agent-0.5.0> --out tests/fixtures/agent-v1
```

| ID | Required behavior | Stage-A status | Evidence / stage |
|---|---|---|---|
| REF-01 | Fixed Agent 0.5 / Protocol 1, no Agent/Runtime crate dependency | **Failed** | No backend crate dependency (Passed). But the production TUI rejects Agent 0.5.0 at bootstrap: measured `unsupported agent version '0.5.0'`; `tests/baseline_defects.rs::baseline_bootstrap_rejects_agent_0_5_protocol_v1`; the 18 real-Agent E2E scenarios all fail. B |
| REF-02 | Protocol version + capability check; no 0.3 fallback | **Failed** | Old code checks only the package string and has a 0.3 fallback behavior; `baseline_ping_result_ignores_protocol_version_and_capabilities`. B |
| REF-03 | Extended reasoning kept, no silent downgrade | **Not run** | Wired types exist; unverified against 0.5.0 because E2E is blocked. B |
| REF-04 | Response/Event interleave; partial frame and EOF | **Passed** | `src/rpc.rs` transport tests run and pass. |
| REF-05 | Send-full does not block UI; draft kept | **Failed** | `tests/backpressure_baseline.rs::baseline_send_blocks_when_the_outbound_queue_is_full` demonstrates the awaiting send blocks. B |
| REF-06 | Control reserve does not reorder queued same-session requests | **Failed** | No control reserve exists (`baseline_queue_capacity_constant`). B |
| REF-07 | read/deferred/byte budgets; expired query still counted | **Not run** | No query-slot machinery exists. B/C |
| REF-08 | Async side effects off the input/RPC loop | **Failed** | `baseline_run_commands_awaits_send_and_clipboard` shows inline await/send. C |
| REF-09 | session.read decodes real Runtime items, cross-page UTF-8 | **Not run** (wire samples pinned) | The **production TUI has no decoder**; only the fixture decoder test passes. `tests/agent_v1_fixtures.rs::session_read_chunks_are_runtime_item_envelopes`. B |
| REF-10 | Pinned prefix / non-zero cursor / new pin legal | **Not run** (wire samples pinned) | Fixture facts only; no production pin logic. B |
| REF-11 | Session-global vs Turn-local index not mixed | **Not run** (wire samples pinned) | Fixture facts only; production uses global offsets. B |
| REF-12 | Read-only browse does not open Session/require Workspace | **Not run** | No browse path. B/E |
| REF-13 | Large/missing/records_truncated/trailing_incomplete not faked complete | **Not run** (wire samples pinned) | `trailing_incomplete` fixture pinned; no production handling. B |
| REF-14 | send may be deferred; preparation visible, no auto-resend | **Failed** | `tests/preparing_baseline.rs` shows no Preparing state and no deferred admission. B |
| REF-15 | Cancel routes by exact operation ID or TurnRef | **Failed** | `baseline_cannot_cancel_a_preparing_submission_before_turn_ref`; no operation routing. B |
| REF-16 | Manual compact four results + unknown_write | **Not run** (two real results pinned) | Real `noop` and `compacted` fixtures captured (`manual_compaction_reports_noop_and_compacted`); failed/unknown_write synthesized. B |
| REF-17 | Context estimate scope; utility usage separate | **Not run** (wire samples pinned) | `session-context-idle`, `session-context-after-compact`. B/E |
| REF-18 | Update next Request; current Tool labels unchanged | **Not run** | Unverified against 0.5.0. B |
| REF-19 | Steer accepted/applied/recorded separated; no cross-Loop prompt | **Failed** | Old `advance_steer_queue_for` promotes an unsent steer into a new `turn.send` (handoff). E2E 7/9/10/11 pass but do not cover the auto-promotion; B/E |
| REF-20 | ACK does not clear a newer draft revision | **Passed (existing)** | `tests/app_flow.rs` steer/editor revision tests. |
| REF-21 | wait failure/lost event recovers via turn.result, no tool rerun | **Not run** | No `turn.result` path. B |
| REF-22 | Failed save retained result readable; Blocked/unknown correct | **Failed** | Old code only latches `result_unconfirmed` and reopens from Store; no `turn.result` retained read (`baseline_has_no_turn_result_recovery`). B |
| REF-23 | Cancel does not claim file rollback; close keeps in-flight result | **Passed (existing)** | `tests/app_flow.rs` lifecycle tests. |
| REF-24 | reload does not clear History/Live/draft or reinstall history | **Failed** | `baseline_reload_stages_a_full_history_replacement` shows a staged full-history replacement. C |
| REF-25 | Per-session drafts/undo/paste/cursor independent | **Failed** | Draft state is one global `App.composer`, not per-session; switching sessions does not preserve an independent per-session draft. D |
| REF-26 | New/continue/rename/delete explicit, no cross-project guess | **Not run** | Reducer tests (`tests/app_flow.rs`) cover rename/delete but E2E 3 is blocked against 0.5.0, so it is not measured end-to-end. C/D |
| REF-27 | Single Arc body; single Tool index | **Not run** | Reload still duplicates bodies. C |
| REF-28 | Live update zero full-history clone; zero stable re-layout | **Failed** | `tests/performance.rs` shows full-history materialization; no counters yet. C |
| REF-29 | viewport/click/copy share layout; softwrap adds no copy newline | **Not run** | Shared layout not yet implemented. C |
| REF-30 | Fold/resize/page-load keep anchor | **Not run** | No `{SectionId, offset, row}` anchor. C |
| REF-31 | Rail geometry / single-line Footer baseline | **Passed** | `src/ui/snapshots.rs` (27), `tests/rail_fixtures.rs`, `tests/render_snapshots.rs` all pass unchanged. |
| REF-32 | Panel/focus does not misfire send/cancel | **Not run** | Panels not yet implemented. E |
| REF-33 | Tool cards not reordered by completion; no duplicate insert | **Passed (existing)** | `tests/app_flow.rs` tool ordering. |
| REF-34 | tool.read distinguishes awaiting_policy/running/terminal | **Not run** (wire samples pinned) | Production has no `tool.read`. `tests/agent_v1_fixtures.rs::tool_read_distinguishes_awaiting_policy_running_and_terminal`. B |
| REF-35 | base64/raw offset/UTF-8 tail/gap/EOF correct | **Not run** (wire samples pinned) | `tool_output_streams_use_raw_byte_offsets`, empty-EOF test; gap/eviction synthesized. B |
| REF-36 | stdout/stderr no fake total order; stop request ≠ confirmed | **Not run** (wire samples pinned) | `tool-read-terminal` `termination_confirmed`; production has no stream layer. B |
| REF-37 | Only visible tools read on demand; hidden stop; partial/expired visible | **Not run** | Tool detail view not implemented. E |
| REF-38 | @file is a path reference; preview does not attach content | **Not run** | Not implemented. D/E |
| REF-39 | workspace.files/search partial + cursor rules | **Not run** (wire samples pinned) | `workspace_files_and_search_report_partial_pages`. B/E |
| REF-40 | workspace.read same-revision in-line paging; changed not spliced | **Not run** (wire samples pinned) | `workspace_read_statuses_are_distinct`. E |
| REF-41 | Changes workspace/tool origin + three comparisons | **Not run** (wire samples pinned) | `changes_list_and_diff_keep_opaque_refs_and_structured_hunks`. B/E |
| REF-42 | Opaque change_ref not parsed; stale/fragment correct | **Not run** (wire samples pinned) | `stale_changes_cursor_is_reported_not_continued`; production has no changes path. B |
| REF-43 | Footer branch from explicit status; renderer does no IO | **Not run** | Not implemented. E |
| REF-44 | Search coverage note; unloaded/large not "global no match" | **Not run** | Not implemented. D |
| REF-45 | Prompt jump; temporary fold does not break selection | **Not run** | Not implemented. D |
| REF-46 | Copy/export no Rail/fake newline; export fixed pin, bounded memory | **Not run** | No export. D |
| REF-47 | External editor: background RPC continues; no draft overwrite | **Not run** | No editor job. D |
| REF-48 | ANSI/OSC/control safe display; backend offset unchanged | **Passed (existing)** | `src/markdown.rs`/display tests. |
| REF-49 | Logs contain no message/command/result/file/secret | **Passed (existing)** | debug-log tests, E2E safety checker. |
| REF-50 | All cache/queue bounded; background sessions release | **Failed** | No bounded cache/queue eviction exists; outbound queue is an unbounded-until-full channel. C |
| REF-51 | Existing CJK/IME/mouse/scrollbar/Terminal restore preserved | **Passed** | `ui::*`, `tests/terminal_restore.rs`, `tests/rail_fixtures.rs`. |
| REF-52 | Common command table/completion/help consistent | **Failed** | Three separate lists (`parse_command`, `SLASH_COMMAND_NAMES`, help text) diverge; `/refresh` missing. D |
| REF-53 | No approval/plugin/Subagent/PTY/Git-write/auto-reconnect | **Passed** | Source audit: no such code. |
| REF-54 | Fixed-Agent E2E covers read/tool/compact/file/diff | **Failed** | The 18 E2E scenarios all fail at bootstrap against Agent 0.5.0 (measured); no read/compact/file/diff scenario exists. B/E |
| REF-55 | Rust 1.85/stable, three-platform original tests pass | **Not run** | Stage A ran only remote Linux 1.97.1; no 1.85/macOS/Windows run. F |
| REF-56 | Release perf before/after with real data, not faked | **Not run** | Before-data recorded in `docs/performance.md` (real remote logs); after is pending. C/F |

## Stage-A status counts

- **Passed** (complete + measured): REF-04, 20, 23, 31, 33, 48, 49, 51, 53.
- **Failed** (demonstrated old violation): REF-01, 02, 05, 06, 08, 14, 15, 19,
  22, 24, 25, 28, 50, 52, 54.
- **Not run** (no implementation yet; wire samples pinned but **not** counted as
  passes): REF-03, 07, 09, 10, 11, 12, 13, 16, 17, 18, 21, 26, 27, 29, 30, 32,
  34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 55, 56.
Fixture samples are **evidence of the backend contract**, not evidence that the
production TUI implements the behavior; those rows are `Not run` on purpose.

## Measured stage-A blocker

Running `tests/agent_e2e.rs` against the pinned Agent 0.5.0 binary fails at
bootstrap with `unsupported agent version '0.5.0'`. Every E2E scenario is
blocked until stage B removes the 0.3.x gate.
