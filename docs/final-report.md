# Final Report: Agent v0.3 / Runtime v0.4 Migration

The current delivery is **0.2.1**. See [release-0.2.1.md](release-0.2.1.md) for
the post-migration reasoning Markdown/order fixes, 277-test remote suites,
and updated macOS package. The migration evidence below remains a historical
record of **0.2.0**; its test counts and cross-target checks are not relabeled
as new runs. Agent/Runtime pins and protocol compatibility are unchanged.

## Delivery Identity

| Repository | Starting HEAD | Final HEAD |
|---|---|---|
| TUI | `3ecb509e353a666711b11bfdee7f50bdc92fe674` | `3ecb509e353a666711b11bfdee7f50bdc92fe674` + uncommitted migration |
| Agent 0.3.0 | `b2e23938d073ab21c2775faa623561ba929a5ed1` | unchanged |
| Runtime 0.4.0 | `87f3cf92b9b5980b0f468174a319cf53427d858e` | unchanged |

The user's Agent reference `c362446a…` was older than the actual starting
`dev HEAD`. This migration uses the actual `b2e23938…`, matching the r2 baseline.
- Migration baseline package: `0.2.0`; supported wire range: Agent `0.3.x` only
- No Agent or Runtime source was modified. No commit was created.

## Implemented Semantics

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

## MIG Coverage

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

The matrix reports **157 PASS / 3 NOT RUN**. The parent independently verified
pins, backend build provenance, dependency absence, and evidence recording
(MIG-001/002/006/007/141/160) by source/metadata audit, which is appropriate for
those requirements and is not a runtime SHA-attestation claim.
MIG-138/139/140 are NOT RUN: GitHub Actions Linux/macOS/Windows jobs were not
triggered. Remote Linux tests and cross-target checks are not substituted for CI.

## Final6 & Post-review Verification

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
