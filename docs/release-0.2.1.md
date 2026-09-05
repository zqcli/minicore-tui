# minicore-tui 0.2.1

This release fixes two reasoning-display regressions in the Agent v0.3 / Runtime v0.4 migration.

## Fixes

- Live and durable reasoning now use the existing `MarkdownRenderer`, while preserving the reasoning-level muted/italic presentation, hidden `Thinking...` behavior, empty-content behavior, section padding, CJK display width, and Markdown colors for headings, lists, inline code, and fenced code.
- The History projection now stores each Assistant block's `Reasoning` part before its `Text` part. Ordering is corrected only within an Assistant block; request boundaries, tool events, and multi-request loop semantics are unchanged.
- The reasoning-layer style adapter supplies the muted foreground only to spans without an explicit Markdown color, avoiding the `Style::patch` foreground precedence that otherwise hides code/list colors.

## Regression Evidence

The red reproduction was run against the pre-fix behavior with synthetic protocol/history data only:

- Remote red log: `/root/minicore-tui-r2-01a06ec1/logs/reasoning-021-red.log`
- Expected failures: 2 (`live_reasoning_renders_markdown_before_each_request_text`, `persisted_reasoning_preserves_markdown_and_request_order_after_reopen`)

The focused green coverage includes four `app_flow` tests for live rendering, persisted/reopened ordering, late reasoning deltas, Markdown elements, hidden rendering, cache/fallback parity, dark/light themes, empty content, and CJK width:

- Remote focused green log: `/root/minicore-tui-r2-01a06ec1/logs/reasoning-021-green.log`
- Result: 4 passed, 0 failed
- Existing reasoning component coverage: 2 passed, 0 failed
- Snapshot suite: 47 passed, 0 failed

## Verification

All Rust verification was performed on the remote builder with locked offline dependencies:

- `cargo +1.85.0 test --locked --offline --all-targets`: 277 passed, 0 failed, 8 ignored
- `cargo +stable test --locked --offline --all-targets`: 277 passed, 0 failed, 8 ignored
- Release version gate: 2 passed, 0 failed
- MSRV and stable `cargo fmt --all -- --check`: passed
- Stable `cargo clippy --locked --offline --all-targets -- -D warnings`: passed
- Stable `cargo doc --locked --offline --no-deps`: passed
- Agent loopback E2E: 7 passed, 0 failed
- Real PTY terminal restore: 1 passed, 0 failed

Final verification logs:

- `/root/minicore-tui-r2-01a06ec1/logs/reasoning-021-all-final.log`
- `/root/minicore-tui-r2-01a06ec1/logs/reasoning-021-quality-final.log`
- `/root/minicore-tui-r2-01a06ec1/logs/reasoning-021-agent-e2e.log`
- `/root/minicore-tui-r2-01a06ec1/logs/reasoning-021-pty.log`

Local copies of the red/green, final suites, quality, E2E, PTY, build, and Mach-O logs are under `docs/verification/reasoning-021-*.log`. The parent independently read the raw final results; the two expected failures remain in the red log and are not failures of the fixed release. An independent review found no production-code defects; stale Markdown/cache documentation and delivery-version references were corrected before packaging.

## macOS Artifact

The TUI release executable was cross-built remotely for `x86_64-apple-darwin` with macOS deployment target `11.0`, then locally ad-hoc signed and verified before atomic replacement of the package executable. No local Rust build was performed.

- Installed artifact: `target/macos-test-package/minicore-tui`
- Signed SHA256: `4ed9c91ea07d81ecac06452f247889cca8b016d5cb3cad51b32e07383cf4c11d`
- Signed size: `3,543,808` bytes
- Format: Mach-O 64-bit x86_64 executable; `--version`: `minicore-tui 0.2.1`
- Code signature: valid ad-hoc signature; not Apple-notarized or Developer ID signed
- Original unsigned build SHA256: `bf32021c4ac036365cd2d9f69c964c797133cd43a6ff67eb219825829d991ec3`
- Package: `target/minicore-tui-0.2.1-macos-x86_64.tar.gz` (excludes user data, workspace, credentials, and backups)
- Launcher: `target/macos-test-package/run.command`; provider/model remains `cus-resp/gpt-5.6-luna`, reasoning `high`
- Build log: `docs/verification/reasoning-021-macos-tui-build.log`
- Unsigned Mach-O inspection log: `docs/verification/reasoning-021-macos-macho.log`

The pinned `minicore-agent` binary and the Agent/Runtime source trees were not modified or rebuilt for this release round. Agent SHA256 remains `0bd1c61469200fc56fca70f3a4bd1a1b6e29e476f5cedd90f451fe1f4183e622`. Existing package data, workspace, and configuration are preserved; the previous TUI executable is retained outside the package as `target/minicore-tui-0.2.0-backup`.

## Native Replay

On Intel macOS 15.7.3, the parent ran the signed 0.2.1 TUI and unchanged Agent in a real PTY using a temporary, local-only copy of the user's test Store. The test opened `/resume` without sending a turn; a loopback-only endpoint and dummy credential prevented use of the real provider. No user history was uploaded or placed in test fixtures.

- 6 Loops, 11 Assistant entries, 10 reasoning runs: request order and rendered Markdown bold/italic passed.
- All 6 reasoning/answer pairs: reasoning appeared before its own answer.
- `/resume`, `/quit`, exit code 0, alternate-screen restoration, and exact terminal-attribute restoration: passed.
- Original Store file hashes: unchanged; temporary copy removed; external provider calls: 0.
- Evidence: `docs/verification/reasoning-021-native-replay.log` (counts/results only, no conversation content).

The real Luna/high provider smoke belongs to the previous 0.2.0 package delivery and was not repeated for this UI-only release. Full native platform suites and GitHub CI remain unexecuted; this targeted native replay does not substitute for them.
