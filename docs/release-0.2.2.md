# minicore-tui 0.2.2

This release fixes the Rail transcript spacing/line-break contract and adds
full Agent reasoning-level support (`xhigh` / `max` / `ultra`) end to end.

## Changes

### Rail spacing and reasoning line breaks

- **User → Thinking gap**: a user card's internal slate padding is not an
  external spacer. Thinking runs are now vertically padded sections (one
  transparent blank above and below, shared with neighbors by the existing
  boundary logic), so exactly one transparent spacer separates the card from
  the first thinking row. Durable and live paths use the same
  `reasoning_lines_with_fold` entry, so the visible geometry is identical.
- **Reasoning raw newlines**: reasonings now render with a private
  markdown-preserving-breaks mode (`MarkdownRenderer::preserving_breaks`):
  a raw single `\n` is a visual row break instead of being flattened by the
  common-mark soft-break space. Blank paragraphs, bold, code, lists, CJK,
  links, and the raw-line fold threshold are unchanged. Default Assistant and
  User Markdown soft-break behavior is untouched outside Thinking.
- **Consecutive reasoning parts**: durable consecutive `Reasoning` parts now
  join with no forced paragraph break (`concat`), matching how live SSE
  deltas accumulate — `['first','\n','second']` renders as two lines in live,
  final, and history alike.
- **Working-status gap**: while busy, the transcript guarantees one clear
  transparent blank between the last transcript row and the Working/Running
  status row even when content fills the viewport (verified at 60x16, 80x24,
  120x40). Sections that already end in a transparent blank (assistant text)
  do not get a second one; the artificial spacer belongs to no section, so
  ranges/copy/hit tests exclude it consistently.

### Max reasoning support

- `Reasoning` wire type extended to the Agent ladder: `Auto, Disabled, Low,
  Medium, High, XHigh ("xhigh"), Max, Ultra` (mirrors the Runtime's
  `ReasoningPreference`). Existing models that do not advertise these levels
  are unaffected: selectors are driven solely by `model.supported_reasoning`.
- `--reasoning xhigh|max|ultra` CLI accepted; unknown levels still rejected.
- Thinking colors, selector labels/descriptions, and help text extend to the
  new levels.
- Footer `current_reasoning` now prefers the live request metadata while a
  loop runs (never rewritten), and otherwise shows the durable session
  setting — after a `session.update` to `max` the footer reflects it instead
  of a past request's historical level.

## Regression Evidence

Red was captured before any fix on the real renderer/App/DTO paths:

- `section_gap_tests` (new): 4 failing — `thinking_single_newline_is_a_visual_break_not_flattened`,
  `consecutive_reasoning_parts_join_like_live_accumulation`,
  `user_card_then_thinking_has_exactly_one_transparent_spacer`,
  `prepared_transcript_preserves_thinking_newlines_from_wire_entry` (the
  busy-working test passed trivially until it was given a viewport-filling
  transcript, then reproduced). Compile red: `Reasoning::XHigh/Max/Ultra`
  missing.
- `assistant_parts_keep_order_and_share_boundary_padding` and
  `empty_reasoning_renders_nothing_and_does_not_hide_the_next_run` were
  failing expectations for the new hidden-thinking trailing spacer.

Green coverage (focused + real paths): 5 `section_gap_tests`, 3 max
component flows (`reasoning_selector_offers_max_only_when_the_model_advertises_it`,
`selecting_max_updates_the_session_and_footer_shows_max`,
`running_request_metadata_drives_footer_until_the_later_request_uses_max`),
`reasoning_wire_roundtrips_all_agent_levels`,
`reasoning_cli_accepts_all_agent_levels_and_rejects_unknown`, and the new
`tests/protocol.rs` unknown-level gate.

## Verification (local, locked offline)

- `cargo +1.85.0 test --locked --offline --all-targets`: 279 passed, 12 ignored
- `cargo test --locked --offline --all-targets` (stable): 279 passed, 12 ignored
- `cargo fmt --check`, `cargo clippy --locked --offline --all-targets -- -D warnings`,
  `cargo doc --locked --offline --no-deps`: passed
- Real-Agent loopback E2E (`MINICORE_AGENT_BIN=... cargo test --test agent_e2e
  -- --ignored --test-threads=1`): 12 passed, 0 failed, including the new
  `e2e_max_reasoning_ships_literal_max_and_provider_body_carries_it` (the
  provider body carries `reasoning.effort == "max"`).
- Real Agent with the local `cus-resp` config now advertises `max` for
  `cus-resp-luna` (`model.list` over stdio, isolated temp workspace, dummy
  key, no provider call).

## Notes

- The pinned 1d0dd16/Pi 0.84.4 fixtures are not overwritten to match the new
  user-facing spacing contract. The native first-row rail glyph/color/content
  fact gate in `tests/rail_fixtures.rs` still checks the first *content* row
  and documents the explicit thinking-padding divergence.
- The parent's existing Agent startup diagnostic work is unchanged and remains
  available (static `ConfigError` causes with redacted messages).