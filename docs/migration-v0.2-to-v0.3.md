# Migration Guide: minicore-tui v0.2 to v0.3

`minicore-tui` 0.3.0 is a TUI-side refactor paired with the fixed
`minicore-agent` 0.5.0 and `minicore-runtime` 0.4.1 revisions. The TUI still
communicates over stdio JSON-RPC and does not link either backend crate or read
Agent data directly.

## Backend Boundary

- Require Protocol v1 and the fixed capability set from `agent.ping`.
- Use paged `session.read` for application history and read-back. The older
  `session.history` method remains compatibility/diagnostic-only.
- Keep Agent and Runtime source/data outside the TUI package. Provider URLs,
  credentials, profiles, models, tools, and `data_dir` stay in Agent config.
- Retain exact `TurnRef` routing, pinned read cursors, byte offsets, and
  explicit stale/partial/oversized states. A dropped event is reconciled from
  authoritative reads; it is never repaired by writing the Store.

## User-Facing Changes

- Tool details are opened from an exact full Tool identity and page bounded
  input/output/stdout/stderr/result streams without introducing a PTY.
- `@file`, `/files`, `/grep`, file previews, `/diff`, `/context`, and the
  Changes/Diff views are read-only workflows. File content is never silently
  attached to a prompt, and workspace paths are not treated as local roots by
  the TUI.
- `/search full` and `/export` are explicit bounded scans. Full scans use the
  captured `session.read` pin and continuation cursor; incomplete coverage is
  labeled rather than reported as a global no-match or complete export.
- `/settings` stores only TUI preferences. CLI values override the config file;
  Agent executable/config path changes apply at the next startup.
- `/editor` invokes one configured executable plus its argument vector against
  an OS temporary draft. There is no shell parsing, `$EDITOR` fallback chain,
  pty, or arbitrary short deadline. A failed, cancelled, stale, invalid, or
  oversized result keeps the existing draft.

## Lifecycle And Limits

Query slots separate remote ownership from queued intent. Close/reopen/delete
and panel close invalidate waiting, ready, refresh, and detached follow-up state
by session or view scope, while an in-flight request remains registered until
its real response releases its slot. This prevents old pages from being
reissued into a new session epoch. Reopened Tool details coalesce a refresh
against the old ToolKey and issue it after the stale response releases the
slot. A `turn.result` retry reclaims its read slot before emission, and a
never-written wait/result retry is retired at a lifecycle boundary.

The owner budgets remain explicit: history 32 MiB, layout 48 MiB, live output
4 MiB per loop and 16 MiB per session, tool presentation 1 MiB per stream and
16 MiB per session, Composer 256 KiB per draft and 8 MiB total, automatic
history decode 8 MiB per item, and editor readback 256 KiB with an 8 MiB
admission budget.

Terminal suspension for an external editor pauses only terminal input and
painting. RPC readers, `App::update`, background turns/tools/waits/saves, and
session processing continue. Shutdown kills, waits for, and cleans up the
editor process before restoring the terminal.

## Verification

The current code/test/snapshot baseline is
`0aa64c5e4d9211351123db059547beddb15c2cce`. Local rustc 1.98.0 validation
passed 828 tests with no failures and 43 ignored, `tests/app_flow.rs` passed 137/137,
and the six ignored release performance workloads passed. The default Rust tests
remain offline and use fixtures/fake-Agent paths.

Hosted CI is configured to test Rust 1.85.0 and stable on Ubuntu, macOS, and
Windows. A separate Ubuntu job checks out and builds the fixed Agent/Runtime
revisions, then runs the ignored serial `agent_e2e` suite against its loopback
mock without provider credentials. Those hosted jobs and the final-source
remote Agent E2E have not run yet. Native manual terminal interaction, exact RSS
accounting, and oversized real-Agent item generation remain separate acceptance
statuses until those environments are available.

The current detailed matrix is in
[`docs/refactor-acceptance.md`](refactor-acceptance.md); the fixed wire facts
are in [`docs/backend.md`](backend.md).
