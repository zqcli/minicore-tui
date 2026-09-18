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

The stage-B migration consumes: `session.read`, `turn.result`,
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

## Fixtures

`tests/fixtures/agent-v1/` holds desensitized result payloads captured from a
real Agent 0.5.0 process by
[`scripts/generate_agent_v1_fixtures.py`](../scripts/generate_agent_v1_fixtures.py).
The generator starts a loopback OpenAI-Responses mock, a synthetic temp
`data_dir` and workspace, drives the RPC methods, and records only result
payloads (no prompts, credentials, tool bodies, or real paths).
`manifest.json` records the pins, generator, capability list, and the fixture
classes that cannot be reproduced against a healthy real process (preparing
timing, compaction fault injection, store-blocked, stream gap/eviction, scan
deadline, stale cursors, records-truncated). Those gaps are synthesized in
stage B unit tests and disclosed rather than fabricated.

`tests/agent_v1_fixtures.rs` decodes the fixtures and asserts the raw item
envelope shape; it is the stage-B migration's starting contract.
