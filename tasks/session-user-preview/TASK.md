# Session labels from first-user previews

## Scope and checkpoints

- Baseline: `b5af2efaeae57da35d857ed051772a4a1c012b4f`.
- Implementation: `5172ec9f962d51e85c743dad7d2254281b1a36c2`.
- Required Host implementation: `0a1c638b37ba6ec03d9300aea142810bdf918628`
  (Host validation record: `e534e88aa5113d9b33b9c89db4c16eb529132958`).
- Requested worker: `gpt-6-astra`, `max` reasoning. No version changes.

`/sessions` uses a nonblank manual title, then the Host's bounded first-user
preview, then the short Session ID. Manual, preview and ID labels use accent,
subdued and faint theme colors; selection and current-session indication stay
independent. Filtering matches the manual title, preview and full/short ID.

Opening a Session never writes the preview into its title. Rename starts with
the actual title and clearing it uses the existing nullable clear operation.
The first accepted user input refreshes the catalog only while its preview is
missing, including memory Sessions whose model request is still running.
No per-Session history reads or second preview cache are added.

## Verification

Executed against the exact Host implementation above using local-only Cargo
patches, Rust 1.90.0 and the locked dependency graph:

- `cargo fmt --all -- --check`: passed.
- `cargo test --locked --lib session -- --nocapture`: 39 passed.
- `cargo test --locked --lib session_picker_names_stay_stable_after_opening_history -- --nocapture`: passed.
- `cargo clippy --locked --lib -- -D warnings`: passed.

The native test uses the real in-process Host and a controllable model boundary.
It verifies preview discovery during a pending model call, a fresh frontend
catalog before loading history, repeated opening, filtering, rename prefill,
manual rename and clearing back to the preview. Renderer assertions check the
three label colors in both themes, including selection/current state.

## Integration and limits

The integration owner must update the Host dependency pin to a commit containing
the required Host change. The validation patch and temporary lockfile changes
are not committed. This task does not claim a full-workspace gate, live-model
validation, remote publication or independent review. Other editor/picker tasks
are integrated and validated separately.
