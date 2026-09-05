# Migration Guide: minicore-tui v0.1 to v0.2

This document describes the architectural, protocol, and behavioral changes introduced in `minicore-tui` v0.2.0 to adapt to `minicore-agent` v0.3 (`b2e23938d073ab21c2775faa623561ba929a5ed1`) and `minicore-runtime` v0.4 (`87f3cf92b9b5980b0f468174a319cf53427d858e`).

> **Specification Provenance Note**: An earlier pre-r2 draft v0.2 specification was completely superseded and replaced by `minicore-tui-v0.2-agent-v0.3-runtime-v0.4-migration-spec-r2.md` (Spec r2). That earlier draft specification document is not present in this worktree/repository checkout, and this checkout does not provide the superseded original text. All implementation, protocol, and acceptance behaviors are strictly governed by Spec r2.

---

## 1. Motivation & Version Gating

`minicore-agent` v0.3 introduced breaking changes to the JSON-RPC interface:
- Transition from durable turn terminals and conversation sequence counters to loop-centric turns identified by `loop_id`.
- Dynamic mid-turn steering (`turn.steer`) and session configuration updates (`session.update`) taking effect at request boundaries.
- Paginated indexed session history (`session.history`) replacing the monolithic `session.transcript`.
- Explicit turn persistence status tracking (`persisted` vs `failed`) with session blocking on failure.

`minicore-tui` v0.2 enforces a strict version gate in `agent.ping`:
- Only `0.3.x` versions are accepted.
- Versions `0.2.x` and `>= 0.4.0` are rejected, latching the connection into a `Failed` state.
- The TUI does not maintain dual-protocol adapters (`AgentV02Adapter`), nor does it take direct Rust crate dependencies on `minicore-agent` or `minicore-runtime`.

---

## 2. Legacy Concept Elimination

The following legacy concepts from v0.1 have been eliminated:

| Legacy Concept | Replacement in v0.2 | Reason |
|---|---|---|
| `instance_id` | `TurnRef { session_id, loop_id }` | Turns are identified by `loop_id` assigned by the Agent; instance identity is no longer exposed. |
| `session.transcript` | `session.history` | Monolithic transcript replaced with 0-indexed, paginated history (20 items per page). |
| `TurnTerminalWire` | `TurnResultViewWire` | Terminals replaced by direct turn result views returned by `turn.wait` with explicit outcome and persistence status. |
| `ConversationSeq` | `IndexedHistoryItemWire.index` | 0-based contiguous item indexing across prompt, steering, assistant, tool, and summary items. |
| `SessionStatus::Closing` | Local close/shutdown lifecycle | The Agent wire state has exactly `Idle`, `Running`, `WaitingForInput`, `Finishing`, and `Blocked`; the TUI tracks close/shutdown separately. |
| Unfinished turn repair | Authoritative history reconciliation | Incomplete runs are reconciled against persisted history; TUI never mutates or repairs disk stores. |
| Fixed model assumption | `session.update` mid-turn | Sessions support model and reasoning updates during `Idle` and `Running` states. |

---

## 3. Core Protocol & State Machine Architecture

### 3.1 Session Lifecycle States
`minicore-tui` models the five Agent v0.3 wire session states:
1. `Idle`: Active session ready for a new prompt or configuration update.
2. `Running`: A loop is executing; its active-loop object carries `loop_id`,
   loop status, and current `request_index`.
3. `WaitingForInput`: The Agent requires an interaction this TUI does not
   implement; the TUI shows an unsupported-interaction notice.
4. `Finishing`: Runtime execution has ended and the Agent is completing and
   persisting the loop.
5. `Blocked`: Persistence or internal failure prevents new mutations.

The new-session form and the pending `turn.send` submission are TUI-local
states, not additional Agent wire session statuses. Session close/shutdown is
also tracked outside this five-value wire enum.

### 3.2 Dynamic Configuration Updates (`session.update`)
- During `Idle`, confirming model or reasoning in the selector sends `session.update`. On success, the session info is updated immediately.
- During `Running`, confirming model or reasoning sends `session.update` and records a `PendingConfigUpdate`.
  - Running tool batches retain the old configuration revision.
  - The new configuration applies at the next request boundary in the same loop.
  - Handled out-of-order: If `RequestStarted` with the new revision arrives before the `session.update` response, the update is immediately marked `Applied` upon response arrival.

### 3.3 Dynamic Steering (`turn.steer`)
- In `Running` status (`RunningModel` or `RunningTools`), the composer switches to **Steer** mode.
- Submitting steering dispatches `turn.steer { session_id, loop_id, text }`.
- Upon successful RPC acknowledgment, the composer is cleared and the steer is tracked as `Queued`.
- If the Agent returns an error (such as `-32016 steer_queue_full`), composer text is preserved and a warning notice is displayed.
- Steering submission is strictly forbidden in `WaitingForInput`, `Finishing`, or `Blocked` states.
- Authoritative confirmation occurs when `session.history` reconciles and contains a `UserItemKindWire::Steering` item.

### 3.4 Persistence Failures & Blocked Sessions
- `turn.wait` parses `TurnResultViewWire` directly.
- If `persistence == TurnPersistenceWire::Failed`:
  - Session transitions to `Blocked` (`SessionBlockReasonWire::Persistence`).
  - Existing `live` completion state is moved to `UnsavedLoop` rather than discarded.
  - Pending steers are marked `Unconfirmed`.
  - The composer is disabled with a blocked warning.
  - Future `turn.send` or `turn.steer` attempts are rejected locally.
  - In-flight or race `turn.send` calls receiving `-32004 session_blocked` do not destroy the existing `live` or `UnsavedLoop` data.

### 3.5 Paginated History Progression
- `HistoryState` pages backwards or forwards using contiguous item indices (`loaded_count` / `loaded_end`).
- Progression is strictly based on raw history item indices, never presentation block count (`blocks.len()`), preventing tool card multi-block drift.
- Pages are fetched in chunks of 20 items until `next_offset` is `None`.

### 3.6 Lazy Request Handling
- When `OutputDelta` or `ToolStarted` arrives with an unseen `request_index` before `RequestStarted`, a lazy `LiveRequest` is initialized.
- Output is never silently merged into the previous request.
- The `event_gap` flag is latched on the live loop to warn the user of missing intermediate notifications.

---

## 4. UI & Rendering

`minicore-tui` retains its Pi-inspired minimalist terminal design:
- Fullscreen transcript above a fixed dock containing the composer and footer.
- Background-tinted cards for User prompts, dedicated styling for Steering items, and prominent red warning cards for `UnsavedLoop` states.
- Streaming Assistant responses with markdown rendering and italicized reasoning blocks.
- Tool execution cards showing the tool name, bounded progress/result text,
  and outcome status (`success`, `failed`, `denied`, `cancelled`, or input
  provided); the v0.3 History DTO does not expose tool arguments.
- Two-line footer displaying connection status, active session, current request
  configuration, and pending configuration updates.
- Dark and Light theme support toggled at runtime.

---

## 5. Migration Checklist & Status

All 160 migration criteria (MIG-001 through MIG-160) are mapped in
[`docs/acceptance.md`](acceptance.md). Final6 remote verification records 157 criteria as PASS and 3 as NOT RUN.
PARTIAL is reserved for source/provenance evidence or the final evidence
record. GitHub Actions Linux, macOS, and Windows jobs were not run.
