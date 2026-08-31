# RPC Contract Baseline

This document pins the stdio JSON-RPC contract that minicore-tui implements.
It is derived from the fixed minicore-agent baseline below; wire DTOs in this
repository follow this document exactly and must be re-verified together with
the pin when it changes. Desensitized example frames of every DTO live in
`tests/fixtures/protocol/*.json` and are checked by `tests/protocol.rs` against
the production DTOs.

## Baseline Pin

| Item | Value |
|---|---|
| Agent repository | `https://github.com/zqcli/minicore-agent` (branch `dev`) |
| Fixed commit | `6d5e963031159c458212a92c690e515a2ac3761b` |
| Contract doc | `docs/rpc.md` at blob `b8f4d57c6931cad8b99b39fdda0647a2539824a6` |
| RPC version | `0.2.0` (`agent.ping` returns `{"version":"0.2.0"}`) |

Baseline files reviewed at the fixed commit:

| File | Blob |
|---|---|
| `README.md` | `60ee4447f3fcadfabb7c7cd0c4ea5255b6c41054` |
| `docs/rpc.md` | `b8f4d57c6931cad8b99b39fdda0647a2539824a6` |
| `src/rpc/protocol.rs` | `d2a4b56e925e1b18e68ec2eca16b62730dd9f638` |
| `src/event.rs` | `c35a6ce2bda32ff9cb55b9088b1daa0ff60551d7` |
| `src/agent.rs` | `2cf5d98556f9b939735ce1f000aeb4ade7a0eed2` |

## Transport And Framing

- NDJSON over stdin/stdout: one UTF-8 JSON-RPC request per stdin line, one
  complete JSON object per stdout line. Stdout is reserved for RPC; logs go to
  stderr only.
- Request shape: `{"jsonrpc":"2.0","id":1,"method":"...","params":{}}`. `id`
  is required and may be an integer or string; it is returned unchanged.
  `params` may be omitted or must be an object; empty methods accept omitted
  `params` or `{}`.
- Request frames must fit 1 MiB including the newline. The TUI enforces an
  8 MiB inbound frame bound and treats malformed JSON as a fatal protocol
  error rather than resynchronizing.
- Responses carry exactly one of `result` or `error`. Errors use the JSON-RPC
  shape with `data: {"kind": ..., "retryable": ...}` and never include raw
  diagnostics, credentials, prompts, or tool content.

## Methods

| Method | Params | Result |
|---|---|---|
| `agent.ping` | empty | `{"version":"0.2.0"}` |
| `agent.shutdown` | empty | `{"ok":true}` (final frame on success) |
| `profile.list` | empty | `{"profiles":[ProfileInfo]}` sorted by id |
| `model.list` | empty | `{"models":[ModelInfo]}` sorted by id |
| `session.list` | empty | `{"sessions":[SessionInfo]}` in stable id order |
| `session.create` | workspace required; profile/model/reasoning/title optional | `{"session":SessionInfo}` |
| `session.open` | `{"session_id":...}` | `{"session":SessionInfo}` |
| `session.close` | `{"session_id":...}` | `{"ok":true}` |
| `session.delete` | `{"session_id":...}` (closed session) | `{"ok":true}` |
| `session.state` | `{"session_id":...}` (loaded) | SessionState object |
| `session.transcript` | `{"session_id":...,"after":?, "limit":?}` (limit 1..=100, default 100) | `{"entries":[],"next_after":...,"observed_head":...,"complete":...}` |
| `turn.send` | `{"session_id":...,"text":...}` | `{"turn":TurnRef}` |
| `turn.cancel` | TurnRef | `{"cancelled":bool}` |
| `turn.wait` | TurnRef | TurnOutcome |
| `interaction.answer` | compatibility surface only; TUI does not implement approval UI | |

`model` and `reasoning` are frozen when a session is created; there is no way
to change them on an existing session.

## Shared Wire Schemas

- `ProfileInfo`: `id`, `model`, `reasoning`, `tools`, `approval` (retained for
  wire compatibility; TUI does not use it).
- `ModelInfo`: `id`, `model_ref`, `context_window`, `supports_tools`,
  `supported_reasoning`.
- `SessionInfo`: `session_id`, `title` (nullable), `profile`, `workspace`,
  `model`, `reasoning`, `loaded`, `instance_id` (nullable), `created_at`,
  `updated_at`.
- `TurnRef`: `session_id`, `instance_id`, `turn_id` — all always present; the
  exact identity accepted by `turn.cancel` and `turn.wait`.
- `EventMeta`: `session_id`, `instance_id`, `dropped_before` — all always
  present in every event `data` object.
- `SessionState`: `session_id`, `instance_id`, `status` (`idle`, `running`,
  `waiting_for_input`, `closing`), `health` (`healthy` or `degraded`),
  `active_turn` (nullable), `pending_interaction` (nullable), `conversation_seq`,
  `last_terminal` (nullable).
- `TurnOutcome`: `turn_id`, `terminal`, `usage`. Terminal is `completed`,
  `cancelled_by_user`, `cancelled_by_shutdown`, `cancelled_by_restart`,
  `budget_exceeded`, or `{"failed":{"diagnostic":...}}`.
- `Diagnostic`: required `code`, `category`, `retryable`; never carries
  message text.
- `Usage`: always-present object with optional `u64` members
  (`input_tokens`, `output_tokens`, `reasoning_tokens`, `cache_read_tokens`,
  `cache_write_tokens`, `provider_total_tokens`).

## Agent Events

Notifications without an id:

```json
{"jsonrpc":"2.0","method":"agent.event","params":{"type":"...","data":{...}}}
```

Every `data` object contains the full `EventMeta`.

| `type` | `data` fields |
|---|---|
| `session_opened` | `session`, `meta` |
| `session_closed` | `session_id`, `meta` |
| `session_state` | `state`, `meta` |
| `turn_started` | `turn`, `meta` |
| `output_delta` | `turn`, `channel` (`text`/`reasoning`), `delta`, `meta` |
| `tool_started` | `turn`, `tool_call_id`, `tool_name`, `meta` |
| `tool_progress` | `turn`, `tool_call_id`, `progress`, `meta` |
| `tool_finished` | `turn`, `tool_call_id`, `result`, `meta` |
| `interaction_requested` | `session_id`, `interaction`, `meta` |
| `interaction_resolved` | `session_id`, `interaction_id`, `meta` |
| `turn_finished` | `turn`, `outcome`, `meta` |

`tool_finished` result carries only `outcome` and `content_bytes`; durable
content comes from the transcript. `ToolProgress` has nullable `message`,
`completed`, `total`.

## Ordering And Reliability

All output passes through one bounded channel and one writer task, so stdout
lines are complete and never byte-interleaved. The following orderings are
**not** guaranteed:

- a `turn.send` response before the corresponding `turn_started` event;
- a `turn_finished` event before the corresponding `turn.wait` response;
- the final `output_delta` or Tool event before `turn_finished`;
- a deferred `turn.wait` response before responses to later requests.

Events are best effort and may be dropped (`dropped_before` counts the gap
before an event; loss does not replay). The authoritative sources are
`turn.wait`, `session.state`, and `session.transcript`. The TUI registers
`turn.wait` immediately after a successful `turn.send` and reconciles the
transcript after completion; it never treats events as durable history.

`session.transcript` entries are tagged `user_message`, `assistant_message`,
`tool_result`, `summary`, or `turn_terminal`. Assistant tool calls expose only
`tool_call_id`, `name`, and `call_index` — arguments do not exist in the wire
view. `next_after` drives pagination.

## Errors

| Code | Kind |
|---|---:|
| `-32700` | `parse_error` |
| `-32600` | `invalid_request` |
| `-32601` | `method_not_found` |
| `-32602` | `invalid_params` |
| `-32603` | `internal_error` |
| `-32001` | `session_not_found` |
| `-32002` | `session_not_loaded` |
| `-32003` | `session_busy` (retryable) |
| `-32004` | `session_closed` |
| `-32005` | `invalid_state` |
| `-32006` | `interaction_not_found` |
| `-32007` | `turn_not_found` |
| `-32008` | `profile_not_found` |
| `-32009` | `model_not_found` |
| `-32010` | `workspace_error` |
| `-32011` | `store_error` |
| `-32012` | `provider_error` |
| `-32013` | `core_error` (retryability from safe diagnostic) |
| `-32014` | `invalid_session_settings` |

The TUI shows `message`, `data.kind`, and `data.retryable`, never full JSON
debug output, and does not auto-retry `session.create`, `session.open`, or
`turn.send`.