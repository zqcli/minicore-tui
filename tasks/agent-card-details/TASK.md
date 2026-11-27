# Agent dispatch row and task preview corrections

## Scope

Baseline: `b5af2efaeae57da35d857ed051772a4a1c012b4f`.
Worker: `gpt-6-astra`, `max` reasoning.
No version, Host pin, execution protocol or delegation behavior changes.

- T8: Preserve a useful dispatch subject on narrow terminals. Reduce the
  right-hand metadata before sacrificing the task: full facts, short Session
  ID and shortcut, shortcut only, then no metadata. Normal-width rows retain
  their existing layout. The shared row renderer is unchanged.
- T9: Keep the explicit short delegation name separate from the actual task
  prompt. Do not fill `card.task` from `association.name`; existing origin
  arguments, matching message events and native history can fill it later.
  The existing 512-byte prompt preview bound and short row labels are unchanged.

## Evidence

Implementation checkpoint: `cca74f28d51c11bcab56ee1427b30c96e24bfaa5`.

Both regressions were reproduced before the production fix:

- The narrow dispatch renderer returned a completely blank row.
- A real root → named L1 → named L2 delegation showed `helper-b` in
  `card.task`, instead of the actual `Check git log` prompt, even after native
  history reads had settled. The separate short task label was correct.

After the fix, with Rust 1.90.0 and the unchanged locked Host dependency:

- `cargo fmt --all -- --check`: passed.
- `cargo test --locked --lib agent_cards -- --nocapture`: 16 passed
  (9 renderer tests and 7 real in-process Host frontend tests).
- The narrow-row test covers 45, 8, 18, 25, 35 and 80 columns across pending,
  running, succeeded and failed dispatch states, with Unicode names and text.
- The existing nested delegation test now checks that the explicit `helper-b`
  label and actual task prompt both survive observation. Other lifecycle,
  output recovery, follow-up, inspector and terminal-state regressions pass.

Full-workspace/static-analysis gates are intentionally left to the integrated
branch, avoiding a duplicate build queue while the integration owner combines
this change with T3/T12 and the other editor work. No source changes are required
for those independently owned card enumeration/output regrouping fixes.

## Limits

Renderer fixtures verify terminal rows; native frontend regressions use the
real in-process Host and a controllable model. No live model, visual terminal
session, remote publication or independent review is claimed.
