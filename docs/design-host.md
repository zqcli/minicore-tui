# Embedded Host migration

## Scope

TUI 0.12.3 embeds MiniCore Host 0.12.3 at the immutable revision in
`Cargo.toml`. The frontend uses
`HostClient::in_process` and native Host DTOs. Host owns execution, storage,
configuration, tools and credentials. There is no Agent subprocess, replacement
wire protocol, generic backend trait or second business-state owner.

The terminal editor, paste handling, Markdown, tool cards, selection, search,
anchored scrolling, themes, external editor and background Sessions remain.
Webpage, mobile control, browser assets, HTTP/WebSocket listeners, phone
authentication and their configuration and tests are removed. A future WSS
client does not require these removed servers.

## Ownership and identity

- A Session address is the native `(agent_id, session_id)` pair. Navigation
  changes only the frontend selection. Creating another Session does not change
  its Agent's default Session.
- Host identity and incarnation, controller revision, load identity and runner
  revision keep their separate meanings. Async results capture their original
  Session and local epoch; they never follow subsequent navigation.
- State comes from Host snapshots and subscriptions. Mutation receipts report
  admission; they do not synthesize running, saved, closed or deleted state.
- Open/closing/closed, auto/paused and unloaded/loading/loaded/unloading are
  separate. A closed Session cannot be reopened. Loading does not clear pause.
- Agents and Sessions are discovered with all catalog pages. Recent selection
  uses `last_activity_at`, never lexical ID ordering.

## Inputs, tools and observations

- Enter sends steer; the existing follow-up key sends follow-up. Host owns the
  next-step and next-turn queues and their separate FIFO order. Input list reads
  supply queued text. Recall removes the original message and restores its text
  only after confirmed removal.
- Cancellation names the captured Turn or compaction operation. Call timeout
  and client disconnect do not cancel accepted work. Message and operation IDs
  are correlation IDs, not idempotency keys; uncertain inputs are never resent
  automatically.
- Subscriptions install their initial states and output positions. Output uses
  native UTF-8 byte ranges and SDK buffering; gaps are filled to a fixed observed
  end. ResyncRequired ends the old subscription and installs a new baseline.
- Tool cards begin with actual complete arguments. No argument-generation
  preview is implemented. Approval answers keep original interaction, Turn and
  load IDs. Session-wide grants are not fabricated by the frontend.
- Delegated Session and task-message associations are displayed as reported by
  Host. Finishing the parent tool is distinct from finishing the child task.

## History and configuration

- Native history positions order user, assistant, tool-result, partial,
  summary, compaction and terminal records. History pagination and export use a
  fixed upper bound and native cursors. Repeated observation of one position is
  not duplicate content; repeated text at different positions remains distinct.
- Reported usage is labelled with its actual coverage. Missing usage is unknown,
  not zero. A recent response's token count is not current prompt occupancy or a
  whole-Session total.
- UI preferences remain local in `tui.json`. The shared `--config-dir` is
  passed to Host, which reads `host.json`, Role files and Agent declarations at
  startup and explicit reload. Stable declaration keys preserve identity;
  existing Sessions retain their configuration. Dynamic Agent creation copies
  the selected Agent's native configuration without source management metadata.
- Reload requests contain no candidate or path. Host applies declared changes
  through actual owners and reports full or partial results. The TUI refreshes
  actual state after either outcome and never retries partial work implicitly.
- Session configuration changes bind to later Turns. Workspace queries use the
  addressed Host Session. TUI does not read the backend store or run Git/tools
  to replace missing Host APIs.
- Terminal exit requests controlled Host shutdown and waits for actual cleanup.
  There is no timer that silently upgrades graceful shutdown to force.

## Verification and delivery

The frontend keeps these ownership contracts while removing unused
conversion paths and duplicate input bodies. Catalog reads have one coalescing
owner; workspace status is queried independently of scheduling controls.
History installation indexes native identities within each batch and leaves
unchanged blocks and ordering alone. Tool display limits apply before copying
output; complete native arguments and exported history remain available.

Use real embedded Host tests with explicitly named loopback model fixtures for
execution, streaming, tools, approvals, history and shutdown. Preserve terminal
and editor regression coverage; replace Agent-wire and phone-only tests with
tests for actual Host behavior. Run formatting, lint, tests, documentation and
the native-platform CI gates. Record any unavailable gate accurately. Publish
reviewed commits to `dev` without rewriting history or adding co-author trailers.
