# RPC Contract

`minicore-tui` implements a narrow, pinned adapter for one
[`minicore-agent`] child. The TUI never links the Agent or runtime crates and
never calls a provider directly. Any protocol change requires re-reading the
pinned Agent contract and updating the local DTOs and fixtures together.

## Pin

| Item | Value |
|---|---|
| Agent repository | `https://github.com/zqcli/minicore-agent` (`dev`) |
| Agent commit | `b2e23938d073ab21c2775faa623561ba929a5ed1` |
| Runtime commit | `87f3cf92b9b5980b0f468174a319cf53427d858e` |
| RPC version | `0.3.x` |

These values are the compatibility baseline, not a claim that an arbitrary
Agent build is compatible.

## Transport

The child is started as:

```text
minicore-agent --config <agent-config> --stdio
```

Communication is NDJSON over the child's stdin/stdout:

```json
{"jsonrpc":"2.0","id":1,"method":"model.list","params":{}}
```

There is one TUI stdin writer task and one stdout reader task. A complete
request or response occupies one UTF-8 line. Agent stderr is a separate,
bounded log stream; it is never forwarded to the TUI's RPC stdout or directly
to the terminal.

The Agent request line bound is 1 MiB including the newline. The TUI rejects
larger outbound request lines before writing them. The composer rejects input
over 262144 UTF-8 bytes. Inbound frames are bounded to 32 MiB; malformed JSON
or an oversized frame is a fatal protocol error, and
the reader does not scan ahead for a later line. Agent log lines are capped at
4096 bytes on a UTF-8 boundary and the App retains the newest 200 lines.

## Methods

| Method | Parameters | Result used by the TUI |
|---|---|---|
| `agent.ping` | empty | `{"version":"0.3.x"}` |
| `model.list` | empty | model catalog |
| `profile.list` | empty | profile catalog |
| `session.list` | empty | session catalog |
| `session.create` | workspace required; profile/model/reasoning/title optional | created `SessionInfo` |
| `session.open` | `session_id` | opened `SessionInfo` |
| `session.close` | `session_id` | explicit cleanup result; separate wait still determines saving |
| `session.delete` | `session_id` | explicit confirmed deletion result |
| `session.state` | `session_id` | current five-state session view and active loop object |
| `session.update` | `session_id`, optional `model`/`reasoning` (at least one) | updated `SessionInfo` and optional `active_revision` |
| `session.history` | `session_id`, `offset`, `limit` | durable indexed history page (`HistoryPageWire`) |
| `turn.send` | `session_id`, `text` | `TurnRef` (`{session_id, loop_id}`) |
| `turn.wait` | exact `TurnRef` | direct `TurnResultViewWire` (`{turn, outcome, usage, requests, tool_rounds, final_config_revision, persistence}`) |
| `turn.steer` | `session_id`, `loop_id`, `text` | `{"ok":true}` |
| `turn.cancel` | exact `TurnRef` | cancellation result |
| `agent.shutdown` | empty | `{"ok":true}` |

The TUI also understands Agent event notifications for session state/open/
close, turn start/finish, request start, text/reasoning deltas, and tool lifecycle.

## RPC-16 Audit

The complete v0.3 method surface is covered explicitly below. “PASS” means the
method is represented by the production request/response path and covered by a
remote final6 flow or protocol test; it does not claim a separate
real-provider test for every method.

| # | Method | Request/response evidence | Status |
|---:|---|---|---|
| 1 | `agent.ping` | `tests/protocol.rs:ping_builder_matches_json_rpc_shape` | PASS |
| 2 | `model.list` | bootstrap catalog flow; `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | PASS |
| 3 | `profile.list` | bootstrap catalog flow; `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | PASS |
| 4 | `session.list` | bootstrap/session catalog flow; `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | PASS |
| 5 | `session.create` | `src/app.rs:create_session_activates_and_pages_history` | PASS |
| 6 | `session.open` | `tests/app_flow.rs:reopen_invalidates_old_wait_persisted_response` | PASS |
| 7 | `session.close` | `tests/app_flow.rs:close_verification_internal_or_malformed_retains_loaded_state` | PASS |
| 8 | `session.delete` | `tests/app_flow.rs:session_close_and_delete_command_lifecycle` | PASS |
| 9 | `session.state` | `tests/protocol.rs:session_state_uses_an_active_loop_object` | PASS |
| 10 | `session.update` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | PASS |
| 11 | `session.history` | `tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count` | PASS |
| 12 | `turn.send` | `tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live` | PASS |
| 13 | `turn.cancel` | `tests/app_flow.rs:slash_cancel_sends_exact_turn_cancel_and_wait_reconciles` | PASS |
| 14 | `turn.wait` | `tests/protocol.rs:turn_wait_is_a_direct_turn_result_view` | PASS |
| 15 | `turn.steer` | `tests/app_flow.rs:late_steer_ack_after_complete_history_marks_missing_steer_not_recorded` | PASS |
| 16 | `agent.shutdown` | `tests/app_flow.rs:shutdown_drains_after_child_exit_until_rpc_channel_ends` | PASS |

## Correlation And Ordering

Every request has a monotonically increasing local numeric ID. The App inserts
`RequestKind` into `pending_requests` before `AppCommand::Rpc` leaves
`App::update`. Responses are matched by ID, not arrival order. Responses and
notifications may be interleaved. In particular:

- `turn_started` may precede the `turn.send` response;
- `turn_finished` may precede or follow `turn.wait`;
- the final output delta or tool event may be late or missing;
- a wait response can be delayed behind other responses.

After a successful `turn.send`, the TUI registers `turn.wait` immediately in
the same update. A wait result with `persistence=persisted` starts a `session.state`
refresh and incremental `session.history` chain while the session remains loaded.
Failed or unknown completion retains its live/result/gap facts without pretending
that existing History recovers that loop. Raw history item indexes, not rendered block counts, drive pagination;
tool results patch the matching tool call. Live event order is not used
to fabricate durable history.

Every event carries session metadata and `dropped_before`. A positive
value marks an event gap. The TUI displays the gap and clears it only after actual History alignment for
an appropriately confirmed result; failure/unknown retains the marker. It does
not add event ACK, replay, or reconnect
protocols. `turn.wait`, `session.state`, and `session.history` are the
authority. A retained blocked completion may be read once more through the
explicit App refresh path; there is no polling or automatic retry.

## Wire Projection

The local DTOs intentionally cover only fields needed by the TUI:

- models: ID, model reference, context window, tool support, reasoning levels;
- profiles: ID, model, reasoning, tools;
- sessions: identity, title, workspace, profile, model, reasoning, loaded metadata;
- history entries: user, assistant, tool result, and summary;
- session state: status, active loop object, and block reason;
- outcomes: loop outcome, persistence status, request/tool counts, config revision, and safe usage fields;

Unknown fields are tolerated so additive Agent fields do not break the
frontend. Error display uses the safe message/kind/retryable projection and
never prints raw frames, credentials, prompts, or tool content.

## Fixtures And Tests

Desensitized frames live under
[`tests/fixtures/protocol/`](../tests/fixtures/protocol/): model list,
profile list, session create/list/state, history page, output delta, tool started/finished,
turn wait, and RPC error. `tests/protocol.rs` parses them
through production `parse_frame` and DTO code. The unit tests in
`src/rpc.rs` exercise the public `RpcProcess`; `tests/agent_process.rs` is a non-installable fake Agent
used by the production process tests.

The wire ordering rules are exercised by `tests/app_flow.rs`. The optional
real-Agent loopback harness uses an isolated configuration and workspace; it
never requires a real provider credential.

## Shutdown

`agent.shutdown` is the only normal quit request. The App enters
`ShuttingDown`, blocks ordinary follow-ups, tolerates shutdown response/EOF/
child-exit races, and waits for the child. The main loop force-kills after five
seconds if the child does not exit. A failed connection exits without trying
to send another request.

## No Hidden Transport

There is no HTTP, WebSocket, multi-Agent process pool, second RPC client,
store-file parser, shell executor, approval transport, event replay, or
automatic reconnect in this frontend.

[`minicore-agent`]: https://github.com/zqcli/minicore-agent
