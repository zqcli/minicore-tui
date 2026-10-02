# Conversation UX verification — 2026-10-02

This change follows the slash-navigation baseline `58f12c7`.

## Scope

- Literal XML/HTML message fidelity; source-aware fenced-code selection;
  live/durable selection without soft-wrap newlines or UI decorations.
- Collapsed tool action, target, lifecycle, short outcome and process exit status.
  Historical command/file targets survive reopening without extra RPC hydration.
- Rendered-height reasoning folding, keyboard and mouse expansion, truthful
  active/completed labels, and readable dark/light hint colors.
- Markdown tables with a narrow fallback; quoted tables use lossless literal
  display rather than reordering surrounding prose.
- Local fold/resize changes do not manufacture incoming-output notifications.

## Local acceptance

Final native binary SHA-256:
`1b41ecb1f69266b725ceab475295ae4248728040cd4a8f439cb01635721d7df9`.

- Rust stable 1.99 and minimum 1.85: **1,149 passed, 0 failed, 54 ignored** each.
- `cargo fmt --all -- --check`, all-target Clippy with `-D warnings`, and Rustdoc
  with `-D warnings`: passed.
- Fixed Agent/Runtime loopback E2E: **35 passed**.
- Deterministic PTY checks at 60×16, 80×24 and 120×40: **27/27 passed**.
- Process restart/resume checks at all three sizes: **3/3 passed**, with no
  additional model requests during replay. The added test failed the prior
  candidate's missing historical targets before the correction.
- All 52 renderer snapshots passed; intentional label/layout changes were
  reviewed rather than treating the upstream reference text as immutable.

Four real-provider turns covered XML/Markdown, successful and failed reads,
write/patch, and Bash exit 0/7. They completed eight model requests and six
bounded synthetic-fixture tool calls. Native screenshots were visually inspected
in both themes. The final binary then replayed the recorded session to verify
restored targets, warning states, reasoning contrast, and fold/scroll behavior.
The real model was `gpt-6-luna` with requested reasoning `max`, using Responses.

The fixed backend revisions remain those documented in `docs/backend.md`.
The isolated live-test Agent used the operating system's existing trusted CA
roots with TLS verification enabled; production backend code was unchanged.

## Limits

Copy regression tests verify the emitted clipboard payload; successful delivery
to an operating-system clipboard was not exercised in this cloud environment.
The real model's reasoning summaries were short; long-reasoning interactions
were covered by deterministic fixtures. Native screenshot inspection is distinct
from saved text snapshots; native images are not bundled in this record.
Hosted GitHub checks for this new commit are verified separately after push.
