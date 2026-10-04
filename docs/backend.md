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
| TUI | `zqcli/minicore-tui` | See [closeout source provenance](verification/v03-closeout/README.md) | `0.3.0` |
| Agent | `zqcli/minicore-agent` | `d81728b13db68c76b05b4c8cb87161770769947f` | `0.6.0` |
| Runtime | `zqcli/minicore-runtime` | `9e230617d36130e7ec77aba122b45f1347ac53f2` | `0.6.0` |

The Agent and Runtime revisions are fixed inputs for the 0.3.0 release line;
the TUI retains the phase-F lifecycle, ownership, bounds, privacy, shutdown,
Tool Detail, history-reopen, and export-harness fixes. It does not modify either
backend or read the Agent Store. Hosted CI uses exact checkout pins, locked
standalone builds and source-identity checks before the real-Agent loopback
suite. Execution results, as distinct from workflow configuration, belong in
[closeout verification](verification/v03-closeout/README.md).

## Handshake

`agent.ping` returns an ordered capability list plus the protocol version:

```json
{
  "version": "0.6.2",
  "protocol_version": 1,
  "capabilities": [
    "session.read", "session.read.display", "session.context", "turn.result", "tool.read", "tool.read.display",
    "tool.output", "session.history", "workspace.read", "workspace.files",
    "workspace.search", "workspace.status", "changes.list", "changes.diff",
    "deferred.waiter_limit"
  ]
}
```

The refactor requires `protocol_version == 1`. The old `is_supported_agent_version`
package-minor gate (`minor == 3`) is deleted in stage B and replaced by
`validate_backend(protocol_version, capabilities)`. Protocol v1 equality is a
necessary but not sufficient condition; the integrated display-capable Agent build plus this
repository's fixtures/E2E remain the release gate.

`session.read.display` and `tool.read.display` are required together for summary-only
restore and explicit inline-body recovery. Missing either capability stops the
handshake; the TUI never falls back to transferring raw tool bodies on restore.
Historical fixtures below remain decoder evidence, not supported-backend promises.
The fixed-backend CI pins must name the integrated display-capable Agent and its
locked Runtime revision before publication.

Stage B1 (commit `afd2894`) removed the package-minor gate. Fixtures and reducer
tests cover acceptance and rejection of the required Protocol v1 capabilities;
the original 0.5.0 captures below remain historical compatibility fixtures.
The 0.6.0 pair is checked separately through exact-source real-Agent loopback
E2E and lifecycle regressions; see [closeout verification](verification/v03-closeout/README.md).
The earlier 830-test / 34-E2E F-review counts and binary hashes are retained in
[their original report](verification/v03-f/README.md), not current acceptance.

## Compaction and turn ownership (0.6.0)

Ordinary turns remain append-only and soft-threshold crossings never start a
summary call. Only a confirmed safe pre-output upstream context-capacity
rejection may compact and retry the rejected logical request once, within the
active turn's cancellation/deadline and without redispatching completed tools.
Successful completed and persisted turns may reserve independent post-turn
maintenance; `turn.wait` does not wait for that summary. Manual `/compact` is
idle-only. The TUI reads `current_operation` / `last_result` through its bounded
Session context poll owner and cancels independent operations using their exact
`session.compact.cancel` identity, not a completed `turn.cancel`. It adds no
summary policy, provider calls or prompt retries. Turn statistics use `u64`
`tool_rounds`; the legacy execution limit retains zero-means-unlimited semantics.
See [the correlation contract](rpc-contract.md#correlation-and-ordering) for
polling, draft restoration and late-event fences.

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

C2 enforces the local side of those limits (spec §5.2/§5.4 and §21):

- Outbound FIFO: 32 slots total, 28 ordinary + 4 reserved for control
  (`turn.cancel`, `session.compact.cancel`, `session.close`, `agent.shutdown`).
  `try_send` admits or refuses synchronously; a refused request was never
  written, so the app revokes its pending registration and keeps the input.
  Refusals are retried at most twice per exact target (`MAX_SEND_ATTEMPTS`);
  when the bound is reached the app abandons the attempt and restores the
  input — `turn.send` text goes back to the composer (appended after an
  existing draft), a `turn.steer` returns to the front of the paused steer
  queue as unsent, and `session.update`/read intents are dropped with a notice.
  `MAX_REQUEST_LINE_BYTES = 1 MiB` is checked before any write.
- Inbound: 32 MiB per frame plus a 64 MiB aggregate wire budget whose charge
  is released by the app taking ownership of a decoded frame.
- Read/deferred: `app/queries.rs` keeps `QuerySlots::CAPACITY = 2` in-flight
  read-only slots with `MAX_WAITING = 16` queued; `app.rs` keeps at most 16
  deferred `turn.wait`/`turn.result`/`session.compact` intents and coalesces
  retries by exact target.
- Local jobs: `LocalJobs` owns one clipboard job and one serialized decode and
  layout worker, each with bounded admission, result ownership, cancellation,
  and joinable shutdown. Clipboard writes and JSON decoding never run on the
  reducer path. The debug log uses a dedicated writer thread fed by bounded
  `try_send`, truncates each run to 200 lines of at most 4096 bytes, and keeps
  file IO off the UI path; `agent.stderr` frames carry `{bytes, dropped}` only.
- Presentation/history budgets are centralized: history bodies 32 MiB, layout
  cache 48 MiB, live output 4 MiB per loop/16 MiB per session, tool facts 1 MiB
  per stream/16 MiB per session, composer drafts 256 KiB per draft/8 MiB total,
  and automatic typed decoding 8 MiB per item. Oversized items remain explicit
  placeholders on the automatic history path; D2's separate `/export raw`
  workflow streams verified sanitized JSON chunks without raising that ceiling.

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

## Current reproduction

Use the pinned revisions above and the commands in [Testing](testing.md).
Every remote source export must have a recorded content manifest and its own
source-specific target directory; never infer source identity from timestamps
or reuse artifacts from a different export. Record backend commit and binary
hashes alongside the final TUI source hash. The closeout report contains the
actual commands, outcomes and remaining platform limits.

## Historical phase-F reproduction

The following paths, versions, counts and commands describe the earlier F-review
run only; they are not instructions to reuse its targets for current validation.
The exact source under test was the remote checkout at
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
tui    head    9e399d9 (current source; prior F-review daa944a; core baseline 0aa64c5e4d9211351123db059547beddb15c2cce is historical)
CARGO_TARGET_DIR=/root/minicore-tui-v03-refactor/tui-target/f-review-current
```

Verification commands:

```bash
cd /root/minicore-tui-v03-refactor/tui
RUSTUP_TOOLCHAIN=1.85.0 cargo fetch --locked
RUSTUP_TOOLCHAIN=1.85.0 cargo fmt --all -- --check
RUSTUP_TOOLCHAIN=1.85.0 cargo test --locked --offline --all-targets --no-fail-fast
RUSTUP_TOOLCHAIN=1.85.0 cargo clippy --locked --offline --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" RUSTUP_TOOLCHAIN=1.85.0 cargo doc --locked --offline --no-deps
RUSTUP_TOOLCHAIN=1.85.0 cargo test --locked --offline --release --test performance -- --ignored --nocapture
TERMINAL_TEST_BIN=$(find /root/minicore-tui-v03-refactor/tui-target/f-review-current/debug/deps -maxdepth 1 -type f -perm -111 -name 'terminal_restore-*' -print -quit)
FAKE_AGENT_BIN=$(find /root/minicore-tui-v03-refactor/tui-target/f-review-current/debug/deps -maxdepth 1 -type f -perm -111 -name 'agent_process-*' -print -quit)
test -n "$TERMINAL_TEST_BIN" -a -n "$FAKE_AGENT_BIN"
python3 scripts/pty_terminal_validation.py \
  --terminal-test-bin "$TERMINAL_TEST_BIN" \
  --tui-bin /root/minicore-tui-v03-refactor/tui-target/f-review-current/release/minicore-tui \
  --fake-agent-bin "$FAKE_AGENT_BIN" \
  --output /root/minicore-tui-v03-refactor/logs/final-f-review-current/pty-report-with-clipboard.json
```

The authorized remote Rust 1.85/stable current-source F-review runs each pass 830 tests with
53 ignored, strict Clippy, warning-denied rustdoc, and formatting. The isolated
fixed backend job builds the exact Agent/Runtime revisions and passes 34/34
loopback E2Es on both toolchains; the current Release performance set passes
9/9 on both. The Linux
kernel-PTY report passes terminal lifecycle, negative same-slave raw-mode,
input/resize/shutdown, clipboard-child, and idle draw checks. The hosted CI job
builds the fixed Agent and Runtime separately and
reruns the E2E suite with the loopback mock, but no hosted run exists for this
branch. Synthetic frame and editor timings are not terminal input-to-frame
latency measurements.

That run exposed a stale-artifact hazard when rsync preserved source mtimes
in a reused target. Current verification instead uses immutable source exports
and source-specific targets; touching sources is not source-identity evidence.

The 34 Agent E2E scenarios and nine release/performance workloads are ignored
by default and are evidence only when explicitly run with their required
binary/options. The authorized current-source run executed 34/34 E2Es and all
nine performance workloads on both Rust toolchains.
`docs/refactor-acceptance.md` tracks which REF rows remain open.

C2/F status: the B1/B2 lifecycle and `session.read` migration, serialized
bounded decode worker, shared immutable layout worker, `ScrollAnchor`,
viewport/neighbor/recent-result history protection, ToolFacts monotonic
terminal handling, shared body/display owners, bounded SourceMap copy facts,
query-slot invalidation, and the 32 MiB/48 MiB owner budgets are landed. D2
search/copy/export, D3 settings/editor lifecycle, E1 tool details, E2
workspace/file workflows, and E3 Changes/status/Context are implemented. The
current boundary intentionally does not add automatic typed decoding above
8 MiB or claim mathematically exact allocator/RSS accounting; oversized items
remain explicit placeholders on the automatic history path. Native/manual
terminal behavior, hosted CI execution, and real-provider checks remain
separate acceptance evidence rather than being implied here.
