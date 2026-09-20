# Fold transition fix and summary UI status

## Delivered

Production revision: `6b27cff` (fold fix `e552d1c`).

Clicking a durable tool/thinking section invalidated its layout. While the asynchronous worker rebuilt it, the renderer displayed `Preparing conversation...`, clearing the transcript. The pending branch also reported a zero-row viewport, potentially clamping the scroll position.

The fix retains only the last successfully drawn transcript viewport cells until the replacement layout is ready. It does not retain historical layout/body ownership or perform synchronous full-history layout. Input and footer still update. Session ID, epoch, theme and terminal-size barriers prevent replaying another view's cells. Transcript hit testing waits for the current layout to be actually drawn; pending clicks are not replayed against new geometry. Pending work no longer reports a zero-row conversation.

`RuntimeItem::Summary` records that actually exist in history now render as `[compaction] Compaction summary` sections, collapsed by default and expandable to Markdown. Copy/search use the original body, not the decorative label. Fold state is isolated by session/read revision. These UI changes alone do **not** provide real Agent compaction bodies; see the blocker below.

## Verification

All Rust compilation ran on the remote Linux build host with Rust 1.85.0 and locked/offline dependencies. Local macOS only executed downloaded binaries and Python harnesses.

- All-target suite: **854 passed, 0 failed, 53 ignored**.
- Production async-display tests: **5 passed**, covering delayed completion, real mouse toggles, stale events/hits, resize/session/theme/epoch barriers and layout ownership release.
- Summary UI/history tests: **13 passed**, using authoritative synthetic history records; not real compaction-body E2E evidence.
- Fixed-Agent E2E, CI's `--ignored --test-threads=1`: **34 passed**.
- Release performance, `--ignored --test-threads=1`: **9 passed**.
- Formatting, strict Clippy and warning-denied rustdoc: passed.
- All 96 tracked Rust/build-input files matched the remote build sources before installation.

Remote logs: `/root/minicore-tui-v03-refactor/logs/fold-compaction/`.

### Actual terminal clicks

`scripts/fold_display_validation.py` reuses the existing pyte parser, real kernel PTY, fixed Agent and loopback HTTP model. It issues six actual SGR mouse clicks (three collapse/expand cycles), validates the result's visible/hidden state and samples user/assistant anchors.

Both Linux and native Intel macOS produced:

| Observation | Old `b844cf5` executable | Fixed executable |
| --- | ---: | ---: |
| Correct visible/hidden transitions | 3 / 3 | 3 / 3 |
| Placeholder occurrences during clicks | 6 | 0 |
| Samples losing conversation anchors | 6 | 0 |

The old control completed every requested toggle; it was not accepted merely because startup or a click failed. [Native result](macos-fold-summary.json).

Sampling occurs after 10 ms of terminal-output quiescence. There is no terminal draw delimiter, so shorter pure-blank transients could be missed. Raw placeholder detection separately captures the known regression; deterministic delayed-worker tests validate retained cells directly.

The existing ordinary conversation harness also passed on the new native executable: ASCII streaming, Chinese, tool-before/result/after exactly once and in order, and `--continue` restoring the same history. [Native provenance](macos-conversation-PROVENANCE.json). No external Provider request or user Store was involved.

## Installed artifact

- Installed: `/Users/zzq/Develops/minicore-tui/target/debug/minicore-tui`
- SHA-256: `cce6a718114837332b18e1d21113d46bde6957236904f95231e12a335c3ced0b`
- Exact remote artifact: `/root/minicore-tui-v03-refactor/logs/fold-compaction/artifacts/minicore-tui-macos-x86_64`
- Native evidence: `/tmp/minicore-tui-fold-6b27cff/{fold-evidence,conversation-evidence}/`
- Previous binary backup: `/tmp/minicore-tui-fold-6b27cff/minicore-tui-before-install`

The executable was cross-built with Rust 1.85.0, LLVM 19, the existing macOS SDK, target `x86_64-apple-darwin`, deployment target 11.0, incremental compilation disabled and six build jobs. Remote/local hashes, strict codesign verification and local `--version` passed. Agent remains unchanged at 0.5.0; its SHA-256 is `bb9fad0e565e58584dd7169cd8b222d0fe90865f38f89eb32c3d22dfcfe3f371`.

## Blocked: actual compaction summary body

The fixed Agent 0.5 contract deliberately does not expose generated summary bodies:

- Agent `src/sessions/compact.rs:422` commits a separate summary snapshot; publication around line 462 does not append a `HistoryItem::Summary`.
- Agent `src/read.rs:185` onward reads original history, not that snapshot body.
- Agent `docs/rpc.md:377` and `:454` explicitly exclude summary bodies from compaction/context responses.

Therefore a fresh `session.read` cannot supply this feature. The temporary attempt to add such a read was removed, and the existing no-extra-read compaction contract remains tested unchanged. The TUI does not read Agent-private Store files, synthesize a summary, or issue its own summarization request.

The user has been asked whether the previous fixed-backend restriction may be relaxed for a minimal read-only Agent summary interface. No such backend change has been made. A future interface should provide an immutable summary identity, truthful availability/source scope and bounded UTF-8 chunks; same-history re-compaction must not concatenate different summaries. Missing token metadata must not be invented. Live ephemeral summaries need separate lifetime semantics, and a read-only endpoint alone cannot make them survive process restart.

Until that interface is authorized and implemented, **the requested real post-compaction summary section is not complete**. The ready UI for existing summary records must not be presented as proof otherwise.
