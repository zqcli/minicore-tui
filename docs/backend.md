# Backend Contract (v0.3 refactor)

This document fixes the backend facts the v0.3 refactor is built against. It
supersedes the historical r2 provenance that previously lived here (that text
is preserved in git history and in
[the corrective delivery provenance](verification/reload-refresh/README.md)).

The TUI does not link the Agent or Runtime crates, does not read their source
at runtime, and never touches the Agent Store. The Agent executable is supplied
through `--agent-bin`; its config and data directory belong to the Agent.

## Pinned revisions

| Component | Repository | Revision | Package |
|---|---|---|---|
| TUI | `zqcli/minicore-tui` | `9d11ee69c4efa02ef1e5bff143662b48dc3194de` | `0.2.8` |
| Agent | `zqcli/minicore-agent` | `061743369459299e66be97bf97d2b27352a39914` | `0.5.0` |
| Runtime | `zqcli/minicore-runtime` | `6cd2bdbc634437dea925495c61c7eb0be10ba171` | `0.4.1` |

These were re-checked at the start of stage A. All three match the spec's
baseline; no protocol-difference check was triggered. The refactor does not
upgrade dependencies and does not modify the Agent or Runtime source.

## Handshake

`agent.ping` returns an ordered capability list plus the protocol version:

```json
{
  "version": "0.5.0",
  "protocol_version": 1,
  "capabilities": [
    "session.read", "session.context", "turn.result", "tool.read",
    "tool.output", "session.history", "workspace.read", "workspace.files",
    "workspace.search", "workspace.status", "changes.list", "changes.diff",
    "deferred.waiter_limit"
  ]
}
```

The refactor requires `protocol_version == 1`. The old `is_supported_agent_version`
package-minor gate (`minor == 3`) is deleted in stage B and replaced by
`validate_backend(protocol_version, capabilities)`. Protocol v1 equality is a
necessary but not sufficient condition; the fixed Agent 0.5.0 build plus this
repository's fixtures/E2E remain the release gate.

**Current status:** stage B1 (commit `afd2894`) removed the
`is_supported_agent_version` package-minor gate and replaced it with
`validate_backend(protocol_version, capabilities)`, so a pinned Agent 0.5.0
now passes bootstrap. The reducer tests
`bootstrap_accepts_the_pinned_agent_0_5_protocol_v1` and
`bootstrap_rejects_a_backend_missing_required_capabilities` measure both
directions. The 18 real-Agent E2E scenarios are still `#[ignore]`d on this
host because they need `MINICORE_AGENT_BIN` and the loopback mock; protocol v1
equality is a necessary condition, not a release pass.

## Method surface (33 methods)

`agent.ping`, `agent.reload`, `agent.shutdown`,
`profile.list`, `model.list`,
`session.list`, `session.create`, `session.open`, `session.close`,
`session.delete`, `session.state`, `session.context`, `session.compact`,
`session.compact.cancel`, `session.update`, `session.rename`,
`session.history`, `session.read`, `session.presentation`,
`workspace.read`, `workspace.files`, `workspace.search`, `workspace.status`,
`changes.list`, `changes.diff`,
`tool.read`, `tool.output`,
`turn.send`, `turn.steer`, `turn.cancel`, `turn.wait`, `turn.result`,
`interaction.answer`.

The migration consumes: `session.read`, `turn.result`,
`session.context`, `session.compact`/`session.compact.cancel`, `tool.read`,
`tool.output`, `workspace.*`, `changes.*`. `session.history` and
`session.presentation` remain display/diagnostic only; they must not become
the new authoritative read path.

## Read and result contracts

`session.read` returns ordered `utf8_json` chunks of one sanitized Runtime
`HistoryItem` envelope each:

```json
{"item": {"type": "user", "data": {"loop_id": "lup_…", "kind": "prompt",
  "input": {"text": "…"}}}, "timestamp": "2026-…"}
```

The envelope is **not** the legacy `HistoryItemView` display DTO: User carries
`input`, Assistant carries `content` (ordered parts), not `text`/`reasoning`/
`tool_calls`. `captured_end` is a JSONL-prefix boundary (not an item count);
`total` is the readable item count. `history_revision` is the SHA-256 of the
captured prefix; continuation requests carry both. `trailing_incomplete`
reports a half JSONL tail and is never repaired. A large item spans pages; the
next page resumes at `{item, offset}` and `offset`/`total_bytes` are UTF-8
bytes. `max_bytes` bounds the encoded result DTO (default 256 KiB, max 1 MiB).

`turn.result` returns the same chunk shape with an `availability` of
`pending` / `live` / `stored`, plus optional outcome/persistence/usage. Item
indexes are turn-local, not session-global. `turn.wait` is the normal
completion signal and returns a direct result view.

`tool.read` addresses a call by the full
`{session_id, loop_id, request_index, tool_call_id}`. `state` is one of
`requested`/`awaiting_policy`/`running`/`succeeded`/`failed`/`denied`/
`cancelled`/`input_provided`; `awaiting_policy` is never `running` and
`started_at` is set only after the policy decision. `tool.output` streams are
`input`/`output` (`utf8_json`/`utf8`, UTF-8 byte offsets) or `stdout`/`stderr`
(`base64`, raw byte offsets). `base_offset`/`next_offset`/`observed_end` count
decoded bytes, never base64 characters.

`workspace.read` statuses are `ok`/`binary`/`changed`/`too_large`; a page
continues a line byte-exactly via `next_range` + `if_revision`. `workspace.files`
and `workspace.search` return `next_cursor` only when the scan can advance;
`deadline`/`depth`/`entries`/`rules` stop without one. `workspace.status`
reports `repo_available: false` for a definite non-repo and `complete: false`
plus a `warnings` code (never a path) when the observation is partial.

`changes.list` returns opaque `tool:`/`workspace:` `change_ref` values that the
client must not parse; `changes.diff` returns structured hunks whose
`line_complete` fragments concatenate byte-exactly.

## Events

`agent.event` carries `params.data.meta` with `session_id`, optional
`loop_id`, and `dropped_before`. Loop events carry the exact
`{session_id, loop_id}` turn reference. The Agent 0.5 additions the refactor
must decode are `tool_invocation`, `tool_execution`, `tool_process`, and the
current compaction state on `session_state`. Unknown read-only event types are
ignored; known event field corruption is a protocol error.

## Capacity and errors

At most 4 read queries and 32 total deferred waiters/queries run at once;
beyond that the Agent returns `-32019` (`resource_exhausted`, retryable). The
TUI targets 2 read-query slots and ≤16 outstanding deferred requests. Domain
error codes are unchanged from `docs/rpc.md` (`-32001`..`-32022`); error data
carries only `{kind, retryable}` plus a short stable message.

C1 enforces the local side of those limits (spec §5.2/§5.4):

- Outbound FIFO: 32 slots total, 28 ordinary + 4 reserved for control
  (`turn.cancel`, `session.compact.cancel`, `session.close`, `agent.shutdown`).
  `try_send` admits or refuses synchronously; a refused request was never
  written, so the app revokes its pending registration and keeps the input.
  `MAX_REQUEST_LINE_BYTES = 1 MiB` is checked before any write.
- Inbound: 32 MiB per frame plus a 64 MiB aggregate wire budget whose charge
  is released by the app taking ownership of a decoded frame.
- Read/deferred: `app/queries.rs` keeps `QuerySlots::CAPACITY = 2` in-flight
  read-only slots with `MAX_WAITING = 16` queued; `app.rs` keeps at most 16
  deferred `turn.wait`/`turn.result`/`session.compact` intents and coalesces
  retries by exact target.
- Local jobs: clipboard work runs on an owned blocking task with an owner, a
  deadline and a joinable shutdown; the main loop never awaits it.

## Fixtures

`tests/fixtures/agent-v1/` holds desensitized result payloads captured from a
real Agent 0.5.0 process by
[`scripts/generate_agent_v1_fixtures.py`](../scripts/generate_agent_v1_fixtures.py).
The generator starts a loopback OpenAI-Responses mock, a synthetic temp
`data_dir` and workspace, drives the RPC methods, and records only result
payloads (no prompts, credentials, tool bodies, or real paths).
`manifest.json` records the pins, generator, capability list, and a
`provenance` partition:

- `real_process` — payloads captured verbatim from the running 0.5.0 process
  (49 fixtures, including real `session.compact` `noop`/`compacted`,
  `session.context` after compaction, clean-empty-EOF `tool.output`, and a
  stale `changes.list` cursor).
- `source_deterministic` — states the healthy public wire cannot be driven to
  emit, derived from a real captured envelope by changing only fields the
  pinned Agent source documents (`session.context.current_operation`
  `preparing`, `block_reason: persistence`, `CompactionPhase`
  `summarizing`, `CompactionStatus` `failed`/`unknown_write`,
  `ToolDataAvailability` `expired`/`partial`, `WorkspaceScanStop::Deadline`,
  `records_truncated`). Each names its `derived_from` base fixture.
- `not_reproducible_against_the_real_process` — the one class with no fixture
  at all (`tool-output-gap`) and the exact reason; the stage-B stream tests
  cover the decoder path with fault injection.

Nothing is fabricated as if captured: every synthetic fixture carries
`"provenance": "source_deterministic"` and `"derived_from"` in the fixture
file itself. The generator asserts that the manifest lists exactly the JSON
files on disk before exiting.

`tests/agent_v1_fixtures.rs` decodes the fixtures and asserts the raw item
envelope shape; it is the stage-B migration's starting contract.

## Reproduction

The exact source under test is the remote checkout at
`/root/minicore-tui-v03-refactor/tui` on host `192.168.20.199`. Stage A did not
build or test locally. The helper scripts below are session scratch under
`/tmp` and are **not** part of the repository; they hold the host password and
so are never committed.

- Sync local -> remote (excludes `.git/`, `target/`, `snapshots/`,
  `tests/fixtures/agent-v1/`, `*.log`): `/tmp/mctui-sync.sh`.
- Pull regenerated fixtures remote -> local: `/tmp/mctui-pull-fixtures.sh`.
- SSH helper used by both: `ssh-exec` skill at
  `/Users/zzq/.pi/agent/skills/ssh-exec/scripts/ssh-exec.sh`.

Fixed inputs:

```text
agent  binary  /root/minicore-tui-v03-refactor/agent-target/debug/minicore-agent  (0.5.0)
agent  head    061743369459299e66be97bf97d2b27352a39914
runtime head   6cd2bdbc634437dea925495c61c7eb0be10ba171
tui    base    9d11ee69c4efa02ef1e5bff143662b48dc3194de (stage-A baseline)
tui    head    d47d837 (C1 history-read convergence)
CARGO_TARGET_DIR=/root/minicore-tui-v03-refactor/tui-target
```

Verification commands:

```bash
cd /root/minicore-tui-v03-refactor/tui
RUSTUP_TOOLCHAIN=1.85.0 cargo fmt --all -- --check
RUSTUP_TOOLCHAIN=1.85.0 cargo test --locked --all-targets --no-fail-fast
RUSTUP_TOOLCHAIN=1.85.0 cargo clippy --locked --all-targets -- -D warnings
python3 scripts/generate_agent_v1_fixtures.py \
  --agent-bin /root/minicore-tui-v03-refactor/agent-target/debug/minicore-agent \
  --out tests/fixtures/agent-v1
cargo test --release --locked --test performance -- --ignored --nocapture
```

Last verified result (C1, commit `d47d837`): `fmt` clean, `test` 610 passed /
0 failed / 23 ignored, `clippy -D warnings` clean, all under Rust 1.85. Raw
logs: `/root/minicore-tui-v03-refactor/c1-io-tests4.log`,
`c1-history-tests.log`, `c1-io-clippy.log`. Earlier B1/B2 logs are
`b1-*.log` / `b2-*.log`.

The 23 ignored tests are the 18 real-Agent E2E scenarios plus 5 release/perf
tests; they are not evidence. `docs/refactor-acceptance.md` tracks which
REF rows remain open.

C1 status: the synchronous-admission/IO slice (`c843105`) and the
history-read-state convergence (`d47d837`) are landed and verified. The
reload narrowing, the `result_unconfirmed` → `Confirmation` migration, and
the `src/app.rs` module split are **not** landed; the acceptance matrix lists
them explicitly so no partial claim is made.
