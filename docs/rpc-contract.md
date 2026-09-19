# RPC Contract

`minicore-tui` implements a narrow, pinned adapter for one
[`minicore-agent`] child. The TUI never links the Agent or runtime crates and
never calls a provider directly. Any protocol change requires re-reading the
pinned Agent contract and updating the local DTOs and fixtures together.

## Pin

| Item | Value |
|---|---|
| TUI core baseline / F-review remediation | `0aa64c5e4d9211351123db059547beddb15c2cce` / `daa944a` |
| Agent repository | `https://github.com/zqcli/minicore-agent` |
| Agent commit | `061743369459299e66be97bf97d2b27352a39914` (`0.5.0`) |
| Runtime commit | `6cd2bdbc634437dea925495c61c7eb0be10ba171` (`0.4.1`) |
| RPC protocol | `Protocol v1` |

These values are the compatibility baseline, not a claim that an arbitrary
Agent build is compatible. The documentation-only follow-up is separate from
the code/test/snapshot baseline above.

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

The complete Protocol v1 method surface is the one recorded in
[`docs/backend.md`](backend.md):

```text
agent.ping, agent.reload, agent.shutdown,
profile.list, model.list,
session.list, session.create, session.open, session.close, session.delete,
session.state, session.context, session.compact, session.compact.cancel,
session.update, session.rename, session.history, session.read,
session.presentation,
workspace.read, workspace.files, workspace.search, workspace.status,
changes.list, changes.diff,
tool.read, tool.output,
turn.send, turn.steer, turn.cancel, turn.wait, turn.result,
interaction.answer
```

The application-history path is `session.read`. `session.history` and
`session.presentation` remain compatibility/diagnostic reads and are not used
to reconstruct the application transcript.

| Method | Parameters | Result used by the TUI |
|---|---|---|
| `agent.ping` | empty | Protocol version plus ordered capability list |
| `agent.reload` | empty | exactly `{"ok":true}` on success |
| `session.read` | session identity, pinned cursor and byte budget | ordered sanitized Runtime-item chunks with continuation cursor |
| `turn.send` | `session_id`, `text` | exact `TurnRef` (`{session_id, loop_id}`) |
| `turn.wait` | exact `TurnRef` | direct turn result and persistence facts |
| `turn.result` | exact `TurnRef` | pending/live/stored result view |
| `turn.steer` / `turn.cancel` | exact session/turn identity | acknowledgement or cancellation result |
| `tool.read` / `tool.output` | full Tool identity and stream cursor | authoritative facts and bounded stream pages |
| `workspace.*` / `changes.*` | session, scope and opaque cursors | bounded read-only workspace/change observations |
| `session.context` / `session.compact*` | session and operation identity | context/compaction facts and typed outcomes |

`agent.ping` must report `protocol_version == 1` and the required capabilities;
there is no Agent 0.3 package-minor fallback. `agent.reload` is sent with `{}`
parameters and accepts only the exact successful result shape `{"ok":true}`;
malformed, missing, false, or extra fields fail closed. A valid `{"ok":true}`
followed by a catalog/state/read refresh failure reports that configuration
reloaded but the view refresh is incomplete or failed. Transport loss after the
ACK reports configuration as reloaded but leaves the view refresh outcome
unknown; transport loss before the ACK, or a malformed/unknown ACK, reports an
unknown reload outcome and does not automatically retry. Agent event
notifications cover session state/open/close, turn start/finish, request
start, text/reasoning deltas, and tool lifecycle.

A valid `{"ok":false}` reports that configuration was not applied. Agent event
notifications cover session state/open/close, turn start/finish, request start,
text/reasoning deltas, and tool lifecycle.

## Core RPC Audit

The table retains the original focused request/response assertions. The full
Protocol v1 method list is above and the release acceptance matrix records the
broader fixture, reducer, and E2E coverage. “PASS” describes contract or fixture
coverage in the current codebase; it does not claim a real-provider test or a
final-source pinned-Agent run for every method.

| # | Method | Request/response evidence | Status |
|---:|---|---|---|
| 1 | `agent.ping` | `tests/protocol.rs:ping_builder_matches_json_rpc_shape` | PASS |
| 2 | `agent.reload` | `/reload` empty-params reducer path; strict `ReloadResult` DTO | PASS |
| 3 | `model.list` | bootstrap/catalog reload flow; `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | PASS |
| 4 | `profile.list` | bootstrap/catalog reload flow; `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | PASS |
| 5 | `session.list` | bootstrap/session catalog reload flow; `tests/protocol.rs:discovery_fixtures_decode_real_agent_shapes` | PASS |
| 6 | `session.create` | `src/app.rs:create_session_activates_and_pages_history` | PASS |
| 7 | `session.open` | `tests/app_flow.rs:reopen_invalidates_old_wait_persisted_response` | PASS |
| 8 | `session.close` | `tests/app_flow.rs:close_verification_internal_or_malformed_retains_loaded_state` | PASS |
| 9 | `session.delete` | `tests/app_flow.rs:session_close_and_delete_command_lifecycle` | PASS |
| 10 | `session.state` | `tests/protocol.rs:session_state_uses_an_active_loop_object` | PASS |
| 11 | `session.update` | `tests/app_flow.rs:session_update_is_sent_for_an_active_session` | PASS |
| 12 | `session.rename` | `src/ui/component_tests.rs:session_panel_rename_uses_id_and_waits_for_complete_ack`; real Agent path: `tests/agent_e2e.rs:e2e_session_panel_rename_and_delete_against_current_agent` | PASS |
| 13 | `session.read` | `tests/read_chunks.rs` and `tests/app_flow.rs:history_pages_by_contiguous_item_index_not_render_block_count` | PASS |
| 14 | `turn.send` | `tests/app_flow.rs:send_response_registers_direct_wait_and_durable_history_replaces_live` | PASS |
| 15 | `turn.cancel` | `tests/app_flow.rs:slash_cancel_sends_exact_turn_cancel_and_wait_reconciles` | PASS |
| 16 | `turn.wait` | `tests/protocol.rs:turn_wait_is_a_direct_turn_result_view` | PASS |
| 17 | `turn.steer` | `tests/app_flow.rs:late_steer_ack_after_complete_history_marks_missing_steer_not_recorded` | PASS |
| 18 | `agent.shutdown` | `tests/app_flow.rs:shutdown_drains_after_child_exit_until_rpc_channel_ends` | PASS |

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
refresh and incremental pinned `session.read` chain while the session remains loaded.
Failed or unknown completion retains its live/result/gap facts without pretending
that existing History recovers that loop. Raw history item indexes, not rendered block counts, drive pagination;
tool results patch the matching tool call. Live event order is not used
to fabricate durable history.

During `agent.reload`, catalog and active-session reads are staged separately
from the live projection. The public `/reload` sends `agent.reload` first and,
when the active view retains an `unsaved_loop`, `live.reference`, or
`last_result`, registers at most one exact-turn internal `turn.wait` without
waiting for the configuration ACK. That wait reuses normal wait correlation
but remains outside `ReloadProgress`, is never retired as `StaleRead`, and does
not become a reload completion condition. New submit/steer admission and the
automatic steer FIFO are paused; composer text and previously admitted queue
items remain owned by the App. The reload start/end/failure event and a
`ReloadWaitTurn` response or send failure, including one arriving after staging
has ended, do not release the FIFO in the same reducer pass. A later ordinary
event resumes the existing settled/handoff rules. Reads issued before reload
are fenced as `StaleRead`; any retired read or
lifecycle ACK that leaves authority uncertain clears the old session state and
keeps the session's `event_gap`/incomplete-history fence. Recovery issues
independent fresh `session.state` and `session.read` reads. Ordinary pending
reads, history loading, and a temporarily absent SessionState retain their
pre-reload admission behavior.
Reload-retired reads and lifecycle ACKs set a session-scoped
`close_verification_unknown` fence; a matching fresh state response clears it
while the independent history-gap fence remains. The reload-installed state is
also not Steer authority: the App issues a fresh normal `session.state` read,
and only a matching `Running` response for the retained `TurnRef` releases the
Steer fence. Idle notifications and Idle responses cannot release it for the
retained live loop. Once that loop is completed, persisted, history-settled and
Idle, the separate queued fresh-turn handoff remains available exactly once.
After a matching Running pairing, explicit and FIFO `turn.steer` may proceed through an ordinary
History gap, while `turn.send`, lifecycle mutations, and `session.update`
remain protected by the gap. History completion alone cannot authorize a
lifecycle mutation, and a reload failure never treats an old idle snapshot as
renewed close/delete authority. Lifecycle operations are rejected while
staging or while another lifecycle request is pending.

Every event carries session metadata and `dropped_before`. A positive
value marks an event gap. The TUI displays the gap and clears it only after actual History alignment for
an appropriately confirmed result; failure/unknown retains the marker. It does
not add event ACK, replay, or reconnect
protocols. `turn.wait`, `session.state`, and the paged `session.read` path are the
authority; `session.history` is compatibility-only. A retained blocked completion may be read once more through the internal
exact-turn `turn.wait` path; there is no polling or automatic retry.

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
