# minicore-tui 0.2.3

This release fixes the acknowledged session-settings footer display and the
accepted-steer visual contract. It does not change the Agent/Runtime, the
0.2.2 Thinking/spacing geometry, or any source fixtures.

## Changes

### Acknowledged session settings footer

- `current_reasoning` and `current_model` now read **only** the durable
  Agent-acknowledged session snapshot (`view.info`). A successful
  `session.update` response updates `view.info`, so the footer shows the
  selected model/reasoning immediately — idle **and** live — without needing
  a new user message or turn.
- The previous fallbacks (live-request metadata, `last_request`,
  `presentation.model_label`) are no longer used by the footer, which removes
  both the stale-after-update display and any chance of a stale presentation
  cache overriding the current model identity.
- Per-request model/reasoning/`config_revision` metadata remains immutable and
  is not shown in the footer; next-boundary semantics for when the Agent
  actually consumes a change are unchanged. In-flight or rejected updates do
  not claim success: `view.info` only changes on a successful response.

### Accepted steer user-card rendering

- A **Queued** (accepted) pending steer now renders immediately as the shared
  user steering card (`steering_lines`: rail surface, wrap, `accepted_at`
  timestamp; "time pending" when the acknowledgment produced no timestamp),
  followed by a subtle muted `⠸ awaiting history` marker until history
  confirms it.
- A **Persisted** steer is owned by the durable history card; the provisional
  slot is skipped so the steer renders exactly once (no duplicate card/banner
  while the loop is still running).
- **Sending** (in-flight), **NotRecorded**, and **Unconfirmed** keep the
  honest `⠸ Steering (…)` banner with distinct state labels; nothing is
  fabricated about a delivered state (request_started is never treated as
  steer consumption).
- The footer `queued` marker is now shown only while a steer is actually
  pending (**Sending** or **Queued**), so a reconciled **Persisted** steer no
  longer leaves a stale `queued` label.

## Regression evidence

Red was captured first on the real App confirm/response/render path
(`src/ui/settings_footer_steer_tests.rs`, 4 tests, all failing):

- `footer_reflects_acknowledged_reasoning_immediately_while_live`
- `footer_reflects_acknowledged_model_and_rejected_update_keeps_old`
- `accepted_pending_steer_renders_as_user_card_with_subtle_awaiting_marker`
- `persisted_steer_removes_provisional_and_stale_queued`

Green after the fix: the same 4 tests pass, plus the 0.2.2 footer test was
re-scoped to the new contract (`running_request_metadata...` now asserts the
footer shows the acknowledged `max` immediately while the running request's
own reasoning stays `High` and the later request uses `Max`). The four
`steering_*` snapshots were regenerated after the targeted regressions proved
the accepted-steer user-card geometry (they assert the `↪` card and the
`awaiting history` marker, no longer the old banner).

### Review fixes (P1, P2)

Three red copy regressions (actual `prepare_conversation` + `selection_text`
path) were failing before the fix and pass after:

- `folded_thinking_copy_excludes_the_hint_row_in_durable`
- `folded_thinking_copy_excludes_the_hint_row_in_live`
- `accepted_steer_awaiting_marker_is_excluded_from_the_copy`

`section_copy_text` / `section_copy_is_decorative` now share a single
decorative-row helper: a folded Tool/Thinking hint sits at the row before the
closing blank (`rows.end - 2`; Thinking previously used `rows.end - 1`, which
pointed at the trailing blank and left the real hint row copyable), and the
accepted-steer `awaiting history` marker is excluded as the last row of a
provisional User section (kind User with no durable history index), identified
by section metadata, never by string filtering.

P2 (possible identity collision between a provisional steer's global `local_id`
ordinal and a durable User block) was proven NOT to occur on the real App/prepare
path by `provisional_steer_selection_never_rebases_onto_a_wrong_durable_user`:
durable User SectionIds always use ordinal `0` (`section_id`), steer cards use
ordinal `local_id >= 1`, and selection rebasing requires exact ordinal equality,
so a global steer id equal to another durable user's history index cannot be
conflated. At live→history reconciliation the provisional steer selection is
**cleared** (identity cannot be proven safely; documented limitation) rather than
guessed onto any user card.

## Verification (local, locked offline)

- Stable all-target tests: **378 passed / 13 default-ignored**, including 287 lib tests.
- `rustup run 1.85.0 cargo test --locked --offline --all-targets`: **378 passed / 13 default-ignored**.
- `cargo fmt --all -- --check`, all-target Clippy with `-D warnings`, and docs with `RUSTDOCFLAGS=-D warnings`: passed.
- The 12 default-ignored real-Agent loopback E2E tests passed separately; the remaining real-TTY restoration test passed from iTerm2 with both streams verified as terminals.
- Agent all-target tests: **267 passed / 2 ignored**.
- Debug and Release binaries both report `minicore-tui 0.2.3`.

The E2E mock now explicitly uses blocking accepted sockets with its existing
read timeout: on macOS, accepted sockets could inherit the nonblocking listener
flag, causing `WouldBlock` and a silently discarded request. The session-switch
stress test also waits for the Assistant history item before inspecting its
fold state. These are test-only changes, not production RPC changes.

Native **iTerm2 3.6.11** interaction checks passed at 100×32, with additional
80×24 and 60×16 resize checkpoints. The test executed a real bash tool under
`auto`, changed reasoning while it was running, verified the active request
remained `max` while the next request used `high`, displayed an accepted steer
before tool completion, and verified one final steer card. Idle `/model` and
`/reasoning` changes updated the footer without another provider request.
Binary hashes remained unchanged throughout this run.

Evidence: [verification/0.2.3](verification/0.2.3/README.md). Native screen text
and protocol checks are valid; **pixel screenshots are not verified**. The iTerm
windows had `kCGWindowSharingState = 0`, and attempted captures contained only
the desktop. Those PNGs were rejected and are not included as UI evidence.

## Scope notes

- No Agent/Runtime implementation changes in this patch; all provider requests in verification used a synthetic loopback server, not the user's upstream provider.
- At the user's explicit request, the local `cus-resp.agent.toml` now uses `approval = "auto"` for enabled tools. No tool-policy framework or approval UI was added. Luna continues to advertise `max`; other local configuration is preserved.
- The Thinking/spacing geometry from 0.2.2 is retained; this patch corrects its copy-decoration offsets. `markdown.rs`, `reasoning.rs`, and `assistant.rs` are unchanged from 0.2.2 and were independently reviewed.
- The pinned `1d0dd16`/Pi 0.84.4 source fixtures are unchanged; the steer
  user-card contract is a documented user-facing divergence.
- Historical parity logs in `docs/verification/rail/` remain historical.