# Backend Provenance

The TUI wire DTOs are aligned with these local backend source trees:

| Component | Repository / Source | Pinned revision | Package version |
|---|---|---|---|
| Agent | `minicore-agent` | `b2e23938d073ab21c2775faa623561ba929a5ed1` | `0.3.0` |
| Runtime | `minicore-runtime` | `87f3cf92b9b5980b0f468174a319cf53427d858e` | backend dependency of Agent (`0.4.0`) |

## Remote Build & Binary Origin

- **Source Archive**: Git archive from commit `b2e23938d073ab21c2775faa623561ba929a5ed1` transferred to `/root/minicore-tui-r2-01a06ec1/agent`.
- **Compiler Toolchain**: Rust `1.85.0` (`cargo +1.85.0 build --locked --offline`).
- **Binary Output**: `/root/minicore-tui-r2-01a06ec1/agent/target/debug/minicore-agent`.
- **Binary SHA-256**: `bc82d6c0908129e45c3bc48b9e21f020967165e16f912fa61fbc6eb84925aa2d` (parent-measured).
- **Checkout audit**: Agent and Runtime `HEAD` and `dev` resolve to the revisions above at both start and finish. The user's `c362446a…` Agent reference was older than the actual starting `dev HEAD`; the implementation pins actual `b2e23938…`, as required. Neither backend's tracked source changed.
- **Runtime Dependency**: Pinned directly to `87f3cf92b9b5980b0f468174a319cf53427d858e` in `minicore-agent`'s `Cargo.lock`.

## Five Recent Agent Baseline Commits & TUI Impact Audit

The `minicore-agent` baseline `b2e23938d073ab21c2775faa623561ba929a5ed1` includes five recent commits directly relevant to TUI stability and contract boundaries:

1. `b2e23938d073ab21c2775faa623561ba929a5ed1` — `test(write): bound commit deadline gate wait`
   - **Upstream change**: Adds bounded gate/deadline waits to the Write tool test; no production RPC or Write behavior changes.
   - **TUI impact**: TUI test gates and child waits likewise use deadlines and cleanup. The upstream test-only change does not itself make TUI tests pass.
2. `c362446a156dbcc5854930d0dbaac97bb612ba19` — `docs(agent): clarify shutdown and persistence boundaries`
   - **Upstream change**: Clarifies that `persisted` only guarantees appending to the current process store stream, without transaction/fsync durability guarantees.
   - **TUI impact**: Documented in TUI spec/README: TUI never promises disk crash/fsync durability and handles `failed` by cleanly latching `Blocked`.
3. `cc9ddf7436b49d2360ce5fde16b76e81cd52ef92` — `fix(store): validate persisted session settings`
   - **Upstream change**: Enforces strict format validation for persisted session configurations upon store loading; invalid records return `STORE_ERROR (-32011)`.
   - **TUI impact**: Verified in `regression_test_close_agent_error_single_state_check_and_store_error`; Agent skips invalid records in its list, and TUI preserves healthy sessions and reports explicit open errors without inspecting or mutating Store files.
4. `e511d9e29c75f7d6a7476baec09fc55ca5fcd379` — `test(agent): verify same-loop request-boundary model updates`
   - **Upstream change**: Validates that mid-turn `session.update` takes effect strictly at subsequent model request boundaries (`request_index > 0`).
   - **TUI impact**: Verified in `e2e_scenario_e_same_loop_update` and `deterministic_same_loop_model_a_to_tool_to_model_b`; footer preserves current request config and indicates pending next revision.
5. `bac2b715f7bee3a5865fc581f133dd60acadd1bc` — `fix(session): preserve blocked turn completion`
   - **Upstream change**: Ensures that when turn persistence fails, the in-memory turn completion view is preserved so `turn.wait` returns the actual outcome.
   - **TUI impact**: Verified in `persistence_failure_blocks_without_losing_the_old_result_view`; TUI displays the completed turn content alongside the `UNSAVED TURN` banner and blocked status.

## Upstream Agent Test Verification Status

Upstream agent test records in `/root/minicore-tui-r2-01a06ec1/agent` are provenance only:
- `tests/tui_rpc_flow.rs` (6 tests): Passed remotely.
- `blocked` retained completion tests (2 tests): Passed remotely.
- `same-loop-update` test (1 test): Passed remotely.
- `invalidRecord` test (1 test): Passed remotely.
- **CI Run 33897540665**: Recorded as the upstream spec baseline success run; it is not final6 TUI platform evidence. GitHub Actions Linux, macOS, and Windows jobs were not run for final6.

The TUI does not link either backend crate and does not read their source or
store files at runtime. The Agent executable is supplied separately through
`--agent-bin`; its configuration and data directory are supplied through
`--agent-config` and owned by the Agent process.
