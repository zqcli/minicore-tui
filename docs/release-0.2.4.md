# minicore-tui 0.2.4

Local TUI **0.2.4** / Agent **0.3.2** build. Runtime remains unchanged. Debug
and Release binaries were rebuilt; no commit or release tag was created.
Claims below are bounded by local tests and real iTerm2 interaction evidence,
not current cross-platform or pixel-parity verification.

## Reasoning summary part boundaries (Agent SSE, earliest seam)

The provider streams distinct reasoning summary parts (`response.reasoning_summary_text.delta`
carrying `summary_index`/`item_id`/`output_index`, plus reasoning
`output_item.added`/`done`). The Agent's `openai` parser previously parsed every
reasoning delta as `{delta}` and discarded the boundary fields, so parts were
flattened into one glued reasoning line in live/final/history.

0.2.4 preserves part identity in the parser and inserts exactly ONE newline
between actual nonempty parts/items:

- fragments inside one part stay concatenated (`Plan` + `ning…` -> `Planning…`)
- empty deltas add no line
- existing raw newlines are never doubled
- legacy deltas without any boundary identity remain concatenated exactly as before
- ordinary assistant/refusal text, raw output-item replay/continuation, tool
  flow, and error redaction are unchanged

Verified by six focused Agent parser tests (two `summary_index` in one item,
`item_id` change with index reset, lifecycle fallback without any index, split
and empty deltas, existing newlines, adjacent responses) and the real-Agent E2E
`e2e_reasoning_summary_item_boundaries_survive_to_the_tui`, which now asserts the
durable flattened value equals `"Planning ... caveats\nDetailing ... timeline\nAnalyzing ..."`
and the wire deltas carry each part with a single `\n` separator.

## Steering queue with a true receipt

`turn.steer` responses now carry an optional 1-based `steer_index`; the Agent
emits a read-only `steer_progress {turn, request_index, applied_count}` event at
the real `Model::start`, counting Steering User items present in the PREPARED
prompt history (existing runtime `PromptProvider` seam, no Runtime edits, no new
RPC method). The latest receipt is cached in memory and exposed through the
existing `session.presentation.steer_progress` optional field for lost-event
reconciliation. It proves inclusion in an issued model request, not durable
saving, provider acceptance, or completion of the user's task.

The TUI keeps a per-session bounded FIFO (outside `LiveLoop`, so a finished loop
never drops it):

- Enter during a running loop admits locally, shows the queue, and clears the
  editor only when the submitting revision and text still match (late ACKs never
  clear new content; no silent Sending-guard drop).
- At most ONE steer RPC / accepted-but-unconfirmed item per session until a
  receipt proves the previous one entered a request.
- Receipts are cached if they race ahead of the ACK. Entries become
  `applied_steers` only after their ACK's `steer_index` is covered by the cached
  `applied_count`; the original request position and acceptance time are kept.
  Stale receipts for other loops are ignored and duplicate counts are harmless.
- Mid-loop empty/stale History never marks an accepted steer `NotRecorded`;
  only terminal reconciliation does. Durable History replaces each applied card
  exactly once.
- Cancel / refusal / error / blocked / unsaved / transport loss pause the unsent
  queue (no auto-send or retry of ambiguous accepted messages); a deliberate new
  submission re-opens the gate.
- If a loop seals before the next unsent steer could be sent, the message is
  retained and, after a normal completed + persisted + history-settled idle
  state, starts as a fresh turn (never a resend of an accepted one).
- Alt+Up on an empty editor withdraws the next UNSENT item for editing (a paused
  queue stays recoverable without copying from the display-only dock).

## Queue UI (above Working, one count helper)

Pending steers render as gray `Steering: …` rows ABOVE the Working status row
in the dock, matching the user's reference image. A single
`steer_queue_count`/`steer_queue_rows` helper is the sole source of truth for
both the height reservation and the renderer:

- content lines (bounded to `MAX_DOCK_QUEUE_ROWS` = 3), an explicit `+N more`
  overflow line, and an optional functional Alt+Up hint shown whenever the next
  UNSENT item can actually be withdrawn into the empty editor (the user
  reference shows it in a NORMAL queue too; `paused` is only a prefix). The
  renderer and the height reservation share ONE `hint_label` predicate, so the
  reserved hint row is never mismatched. Never an "edit all" of Agent-accepted
  items.
- the blank gap between queue and Working is its OWN dock row, so layout code
  asserts `queue.bottom() < status.y`.
- the queue is hidden whenever a modal/selector owns the dock, and the footer
  collapses to one row at 60x16 while the editor + transcript stay visible.
- cell-aware truncation never splits a grapheme cluster (full-width CJK or
  multi-codepoint like ZWJ families); an EXACT fit is returned unchanged (never
  silently drops its last cell), width 0 truncates to empty, and over-width
  text ellipsizes at `width - 1` cells. Multiline messages are flattened to one
  display line (the stored text is never truncated).

Queue rows are separate from transcript User slates (no slate background/
border/timestamp). Applied steers render as the real Steering User card until
the durable History card replaces it. The footer `queued N` counts unsent +
in-flight (never applied).

## Receipt pairing by ACK identity (not queue position)

Reconciliation applies an accepted steer ONLY when its own 1-based ACK
`steer_index` is present and `<= ` the monotonic observed `applied_count`
receipt (cached with its first-observed `request_index`). A receipt alone never
applies a `Sending` entry; an ACK index beyond the observed count stays held
until a covering receipt arrives; same-count progress is deduplicated; stale
loop receipts are ignored. `accepted_at` is preserved.

## Text-loss closing (definitive vs ambiguous)

Admission clears the composer only on revision+text match; a channel
`send_failed` restores the message to the Unsent queue in FIFO front and pauses
it; a definite decoded rejection is restored (never silently dropped); an
ambiguous response (parse/malformed) keeps the entry `Unconfirmed` and pauses
WITHOUT ever re-sending it. A `can_send_requests` guard stops all advance while
transport is unavailable.

The fresh-turn handoff (an unsent steer that becomes a new `turn.send` after a
normal completed + persisted + history-settled idle session) treats its failure
by certainty, so it can never double-send a tool-effect bearing message:

- `RpcSendFailed` (never written to the Agent): definitive not-sent -> the item
  returns to `Unsent`, the queue is clamped PAUSED, and the text stays retained
  exactly once (a handoff owns its queued text: no composer duplicate).
- decoded Agent reject of the handoff: definitive -> `Unsent` + paused; the
  editor is NOT duplicated (a handoff already owns its text; only Alt+Up
  withdrawal or an explicit new message can retry it).
- undecodable (Parse/Malformed) response AFTER the request reached the Agent:
  UNCERTAIN -> the item becomes `Unconfirmed` (never `Unsent`, never auto-sent),
  the queue pauses, and the `Unconfirmed` entry BLOCKS the central queue advance
  even after a new deliberate submission releases the pause gate.
- if a `turn_started` event already bound the fresh loop before a malformed
  response arrives, the accept is PROVEN: the running loop is preserved (never
  abandoned or retried) and the queue entry is dropped like the accepted path.

## Tests

- Agent: reasoning boundary tests incl. the new metadata-gap pair (white-box
  `queue_reasoning_delta` parser check + LIVE final SSE check:
  `['Plan'(idA,0),'ning'(none),' phase'(idA,0),'Detail'(1)]` ->
  `"Planning phase\nDetail"`, plus the empty-delta-carries-new-index boundary)
  and the `reasoning_summary_part.added/done` lifecycle branch; full suite green.
- TUI: receipt-race/identity tests (progress-before-ACK, ACK index vs observed
  count held, stale-after-switch, presentation recovery), FIFO
  admission/withdraw, handoff-failure-by-certainty tests (malformed ->
  `Unconfirmed` + blocked advance; definite reject -> `Unsent` + paused + no
  duplicate; pre-write send failure -> `Unsent` + paused; `turn_started` proven
  accept -> loop preserved + entry dropped), dock/footer queue tests,
  queue-above-Working geometry (incl. 60x16 footer + modal hide + explicit
  gap), exact-fit/grapheme/CJK truncation + multiline flatten + normal/paused
  Alt+Up hint; the old regression test that encoded the text-loss behavior was
  updated to the new restore-on-reject semantics.
- Real-Agent E2E (`MINICORE_AGENT_BIN`, loopback mock, `--test-threads=1`):
  16/16 green: the 12 baseline scenarios (incl. `e2e_scenario_e_same_loop_update`
  restored verbatim from HEAD), reasoning boundaries, paced FIFO
  (`e2e_fifo_steers_are_paced_until_receipt`: request 1 carries A and NOT B;
  request 2 carries A + B; both persist once), duplicate texts, and the direct
  RPC batch demo `e2e_two_consecutive_steers_both_reach_the_provider` (both
  steers handed straight to the runtime at one boundary batch into a SINGLE
  provider request carrying taskA + taskB).

## Final Local Verification

- TUI stable and Rust 1.85: **401 passed / 17 default-ignored** each.
- Agent stable and Rust 1.85: **279 passed / 2 ignored** each.
- All 16 ignored real-Agent loopback E2E scenarios passed separately.
- The remaining real-TTY enter/restore test passed in iTerm2 with terminal stdin/stdout verified.
- Formatting, all-target Clippy with `-D warnings`, and docs with warnings denied passed in both repositories.
- Native iTerm2 3.6.11 verified three reasoning parts on separate rows despite no raw newlines, two rapid queue submissions above Working, Option+Up withdrawal/re-admission, 60×16 queue visibility, separate Alpha/Beta requests and one final history entry each, and immediate Footer `max` updates.
- Native-run TUI/Agent hashes were unchanged from start to finish. Screen text was restricted to the current terminal rows, excluding resize scrollback. No pixel screenshot is claimed.

See [verification/0.2.4/README.md](verification/0.2.4/README.md) for logs and native evidence.

## Compatibility

Restart both the TUI and its Agent child to use the fixes. Historical reasoning
already stored as a glued string has no reliable part boundaries to recover;
this patch neither guesses those boundaries nor replays old steering messages.

The FIFO admits one new steer per issued model request. A task may span multiple
requests and tools; this is steering the running conversation, not a guarantee
that each independent job finishes before the next instruction. Unsent queues
are process-local. An uncertain delivery remains held rather than automatically
retried or treated as an editable unsent message.

Backward compatible with the previous Agent: older Agents simply omit
`steer_index` and `steer_progress`; the TUI then holds accepted steers until
terminal History / loop end (conservative, never a fake receipt). Pacing is a
TUI queuing decision; the Agent/Runtime batch contract (both steers into one
provider request when handed together) is now proven directly by
`e2e_two_consecutive_steers_both_reach_the_provider`, independent of TUI pacing.