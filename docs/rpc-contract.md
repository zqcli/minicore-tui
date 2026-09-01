# RPC Contract

`minicore-tui` implements a narrow, pinned adapter for one
[`minicore-agent`] child. The TUI never links the Agent or runtime crates and
never calls a provider directly. Any protocol change requires re-reading the
pinned Agent contract and updating the local DTOs and fixtures together.

## Pin

| Item | Value |
|---|---|
| Agent repository | `https://github.com/zqcli/minicore-agent` (`dev`) |
| Agent commit | `6d5e963031159c458212a92c690e515a2ac3761b` |
| RPC version | `0.2.0` |
| `docs/rpc.md` blob | `b8f4d57c6931cad8b99b39fdda0647a2539824a6` |
| Reviewed protocol blob | `d2a4b56e925e1b18e68ec2eca16b62730dd9f638` |
| Reviewed event blob | `c35a6ce2bda32ff9cb55b9088b1daa0ff60551d7` |
| Reviewed Agent blob | `2cf5d98556f9b939735ce1f000aeb4ade7a0eed2` |

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
larger outbound request lines before writing them. Inbound frames are bounded
to 8 MiB; malformed JSON or an oversized frame is a fatal protocol error, and
the reader does not scan ahead for a later line. Agent log lines are capped at
4096 bytes on a UTF-8 boundary and the App retains the newest 200 lines.

## Methods

| Method | Parameters | Result used by the TUI |
|---|---|---|
| `agent.ping` | empty | `{"version":"0.2.0"}` |
| `model.list` | empty | model catalog |
| `profile.list` | empty | profile catalog |
| `session.list` | empty | session catalog |
| `session.create` | workspace required; profile/model/reasoning/title optional | created `SessionInfo` |
| `session.open` | `session_id` | opened `SessionInfo` |
| `session.state` | `session_id` | current state and active turn |
| `session.transcript` | `session_id`, optional `after`, page limit | durable transcript page |
| `turn.send` | `session_id`, `text` | `TurnRef` |
| `turn.wait` | exact `TurnRef` | `TurnOutcome` |
| `turn.cancel` | exact `TurnRef` | cancellation result |
| `agent.shutdown` | empty | `{"ok":true}` |

The TUI also understands Agent event notifications for session state/open/
close, turn start/finish, text/reasoning deltas, and tool lifecycle. The
compatibility-only interaction notifications become an unsupported-interaction
notice; there is no approval UI.

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
the same update. A wait response starts a `session.state` refresh and an
incremental `session.transcript` chain. Durable sequence numbers deduplicate
pages; tool results patch the matching tool call. Live event order is not used
to fabricate durable history.

Every event carries session/instance metadata and `dropped_before`. A positive
value marks an event gap. The TUI displays the gap and heals it through a
durable transcript fetch; it does not add event ACK, replay, or reconnect
protocols. `turn.wait`, `session.state`, and `session.transcript` are the
authority.

## Wire Projection

The local DTOs intentionally cover only fields needed by the TUI:

- models: ID, model reference, context window, tool support, reasoning levels;
- profiles: ID, model, reasoning, tools, with approval retained only for wire
  compatibility;
- sessions: identity, title, workspace, profile, model, reasoning, loaded and
  instance metadata;
- transcript entries: user, assistant, tool result, summary, and turn terminal;
- outcomes: terminal state and safe usage/diagnostic fields.

Unknown fields are tolerated so additive Agent fields do not break the
frontend. Error display uses the safe message/kind/retryable projection and
never prints raw frames, credentials, prompts, or tool content.

## Fixtures And Tests

Desensitized frames live under
[`tests/fixtures/protocol/`](../tests/fixtures/protocol/): model list,
profile list, session create/list/state, transcript page, output delta, tool
started/finished, turn wait, and RPC error. `tests/protocol.rs` parses them
through production `parse_frame` and DTO code. `tests/rpc_io.rs` exercises the
public `RpcProcess`; `tests/agent_process.rs` is a non-installable fake Agent
used by the production process tests.

The wire ordering rules are exercised by `tests/app_flow.rs` and the ignored
real-Agent loopback test in `tests/agent_e2e.rs`. The E2E uses an isolated
configuration and workspace and rejects non-loopback provider endpoints; it
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
