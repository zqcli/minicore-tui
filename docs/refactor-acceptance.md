# v0.3 Refactor Acceptance Matrix (REF-01…REF-56)

Status legend: **Passed** / **Failed** / **Not run** / **Not applicable**.
No percentage estimates. Stage A only establishes the baseline facts, so most
rows are **Not run** with the stage that will cover them named. A row is
**Passed** only when the cited test ran in stage A on the remote builder.

Stage-A evidence commands (remote `/root/minicore-tui-v03-refactor/tui`):

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/generate_agent_v1_fixtures.py --agent-bin <agent-0.5.0> --out tests/fixtures/agent-v1
```

| ID | Required behavior | Stage-A status | Evidence / stage |
|---|---|---|---|
| REF-01 | Fixed Agent 0.5 / Protocol 1, no Agent/Runtime crate dependency | Passed (facts pinned) | `tests/agent_v1_fixtures.rs::manifest_pins_the_fixed_backend_and_protocol`; Cargo.toml has no backend dep |
| REF-02 | Protocol version + capability check; no 0.3 fallback | Not run (stage B) | RED baseline `tests/baseline_defects.rs::baseline_bootstrap_rejects_agent_0_5_protocol_v1`; B adds positive |
| REF-03 | Extended reasoning kept, no silent downgrade | Not run (stage B) | E2E 5 exists; B re-points it |
| REF-04 | Response/Event interleave; partial frame and EOF | Passed (existing) | `src/rpc.rs` transport tests |
| REF-05 | Send-full does not block UI; draft kept | Not run (stage B) | RED baseline `tests/backpressure_baseline.rs::baseline_send_blocks_when_the_outbound_queue_is_full` |
| REF-06 | Control reserve does not reorder queued same-session requests | Not run (stage B) | B |
| REF-07 | read/deferred/byte budgets; expired query still counted | Not run (stage B/C) | B/C |
| REF-08 | Async side effects off the input/RPC loop | Not run (stage C) | RED baseline `tests/baseline_defects.rs::baseline_run_commands_awaits_send_and_clipboard` |
| REF-09 | session.read decodes real Runtime items, cross-page UTF-8 | Passed (decoder fixture) | `tests/agent_v1_fixtures.rs::session_read_chunks_are_runtime_item_envelopes`, `read_pages_reconstruct_exactly_with_continuation_cursors` |
| REF-10 | Pinned prefix / non-zero cursor / new pin legal | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::read_pages_reconstruct_exactly_with_continuation_cursors`; B implements |
| REF-11 | Session-global vs Turn-local index not mixed | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::turn_result_availability_covers_pending_and_stored`; B implements |
| REF-12 | Read-only browse does not open Session/require Workspace | Not run (stage B/E) | `docs/backend.md` fact; B/E |
| REF-13 | Large/missing/records_truncated/trailing_incomplete not faked complete | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::trailing_incomplete_history_is_reported_not_repaired`; B adds records_truncated |
| REF-14 | send may be deferred; preparation visible, no auto-resend | Not run (stage B) | RED baseline `tests/preparing_baseline.rs` |
| REF-15 | Cancel routes by exact operation ID or TurnRef | Not run (stage B) | RED baseline `baseline_cannot_cancel_a_preparing_submission_before_turn_ref` |
| REF-16 | Manual compact four results + unknown_write | Not run (stage B) | B (fixture gap disclosed) |
| REF-17 | Context estimate scope; utility usage separate | Not run (stage B/E) | `session-context-idle` fixture; B/E |
| REF-18 | Update next Request; current Tool labels unchanged | Not run (stage B) | E2E 12/13; B |
| REF-19 | Steer accepted/applied/recorded separated; no cross-Loop prompt | Passed (existing) | `tests/app_flow.rs` steer tests; E2E 7/9/10/11 |
| REF-20 | ACK does not clear a newer draft revision | Passed (existing) | `tests/app_flow.rs`; E2E 11 |
| REF-21 | wait failure/lost event recovers via turn.result, no tool rerun | Not run (stage B) | B |
| REF-22 | Failed save retained result readable; Blocked/unknown correct | Passed (existing) | `tests/app_flow.rs::persistence_failure_blocks_without_losing_the_old_result_view`; B re-points |
| REF-23 | Cancel does not claim file rollback; close keeps in-flight result | Passed (existing) | `tests/app_flow.rs` lifecycle tests |
| REF-24 | reload does not clear History/Live/draft or reinstall history | Not run (stage C) | RED baseline `tests/baseline_defects.rs::baseline_reload_stages_a_full_history_replacement` |
| REF-25 | Per-session drafts/undo/paste/cursor independent | Passed (existing) | `src/state/composer.rs` tests |
| REF-26 | New/continue/rename/delete explicit, no cross-project guess | Passed (existing) | E2E 3, `tests/app_flow.rs` |
| REF-27 | Single Arc body; single Tool index | Not run (stage C) | C |
| REF-28 | Live update zero full-history clone; zero stable re-layout | Not run (stage C) | RED baseline `tests/performance.rs`; C adds counters |
| REF-29 | viewport/click/copy share layout; softwrap adds no copy newline | Not run (stage C) | C |
| REF-30 | Fold/resize/page-load keep anchor | Not run (stage C) | C |
| REF-31 | Rail geometry / single-line Footer baseline | Passed | `src/ui/snapshots.rs` (27), `tests/rail_fixtures.rs`, `tests/render_snapshots.rs` |
| REF-32 | Panel/focus does not misfire send/cancel | Not run (stage E) | E |
| REF-33 | Tool cards not reordered by completion; no duplicate insert | Passed (existing) | `tests/app_flow.rs` tool ordering |
| REF-34 | tool.read distinguishes awaiting_policy/running/terminal | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::tool_read_distinguishes_awaiting_policy_running_and_terminal` |
| REF-35 | base64/raw offset/UTF-8 tail/gap/EOF correct | Passed (fact pinned, partial) | `tests/agent_v1_fixtures.rs::tool_output_streams_use_raw_byte_offsets`; gap/eviction in B |
| REF-36 | stdout/stderr no fake total order; stop request ≠ confirmed | Passed (fact pinned) | `tool-read-terminal` fixture `termination_confirmed`; B |
| REF-37 | Only visible tools read on demand; hidden stop; partial/expired visible | Not run (stage E) | E |
| REF-38 | @file is a path reference; preview does not attach content | Not run (stage D/E) | D/E |
| REF-39 | workspace.files/search partial + cursor rules | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::workspace_files_and_search_report_partial_pages` |
| REF-40 | workspace.read same-revision in-line paging; changed not spliced | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::workspace_read_statuses_are_distinct`; E implements |
| REF-41 | Changes workspace/tool origin + three comparisons | Passed (fact pinned) | `tests/agent_v1_fixtures.rs::changes_list_and_diff_keep_opaque_refs_and_structured_hunks` |
| REF-42 | Opaque change_ref not parsed; stale/fragment correct | Passed (fact pinned, partial) | same; stale in B |
| REF-43 | Footer branch from explicit status; renderer does no IO | Not run (stage E) | E |
| REF-44 | Search coverage note; unloaded/large not "global no match" | Not run (stage D) | D |
| REF-45 | Prompt jump; temporary fold does not break selection | Not run (stage D) | D |
| REF-46 | Copy/export no Rail/fake newline; export fixed pin, bounded memory | Not run (stage D) | D |
| REF-47 | External editor: background RPC continues; no draft overwrite | Not run (stage D) | D |
| REF-48 | ANSI/OSC/control safe display; backend offset unchanged | Passed (existing) | `src/markdown.rs`/display tests |
| REF-49 | Logs contain no message/command/result/file/secret | Passed (existing) | debug-log tests, E2E safety checker |
| REF-50 | All cache/queue bounded; background sessions release | Not run (stage C) | C |
| REF-51 | Existing CJK/IME/mouse/scrollbar/Terminal restore preserved | Passed | `ui::*`, `tests/terminal_restore.rs`, `tests/rail_fixtures.rs` |
| REF-52 | Common command table/completion/help consistent | Not run (stage D) | D |
| REF-53 | No approval/plugin/Subagent/PTY/Git-write/auto-reconnect | Passed (existing, stage A) | source audit; no such code |
| REF-54 | Fixed-Agent E2E covers read/tool/compact/file/diff | Not run (partial) | E2E exists (18) but no read/compact/file/diff scenario yet; B/E |
| REF-55 | Rust 1.85/stable, three-platform original tests pass | Not run | Stage A ran only remote Linux 1.97.1; no 1.85/3-platform run |
| REF-56 | Release perf before/after with real data, not faked | Not run (partial) | before-data recorded in `docs/performance.md`; after pending |

## Stage-A summary

- **Passed** on the stage-A baseline: REF-01, 04, 09, 10, 11, 13, 19, 20, 22,
  23, 25, 26, 31, 33, 34, 35, 36, 39, 40, 41, 42, 48, 49, 51, 53.
- **Not run**: everything gated on stage B–F implementation or on the
  multi-toolchain/multi-platform CI runs.
- No row is marked **Failed**: the RED baseline tests intentionally assert the
  *old* defect and pass on the old code; they are not refactor failures.
