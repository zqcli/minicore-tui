# Native iTerm2 live coding verification — 2026-10-11

## Scope and source identity

This run used a real iTerm2 window and the TUI editor, not direct `turn.send`
RPC injection. API credentials were supplied privately by the user, passed via
an environment variable, and never included in this record or the test source.
The exercise used an isolated workspace and Agent data directory, not an
existing user session or the MiniCore repositories.

Sources pulled before the run:

- TUI: `b9b96221eeb585f93adfd414aa7f588f8c02d424`, on
  `refactor/v0.3-full-project`. Remote `dev` was still `604ded4`; the latest
  user changes were on the refactor branch.
- Agent: `cd642d90ec202c002e67c94c87647b68ef512d2f` (0.6.2).
- Runtime: `666d5a15ac08c614ce75dd90337f95f10ec29f80` (0.6.1), matching the
  Agent's pinned Git dependency.

The original and repaired macOS x86_64 Release binaries were compiled on the
user-authorized Linux builder using Rust 1.85.0, LLVM and the existing macOS
SDK (minimum deployment 11.0). No Rust build or test ran locally. Native TUI
execution and the editor interaction were explicitly requested by the user.

## Live exercise

Model configuration was `gpt-6-luna`, reasoning `max`, through the user's
Responses-compatible service. A models-list request confirmed that exact model
and Responses endpoint support. No alternate model or reasoning fallback was
used for the exercise.

The task implemented a standard-library Python JSONL reporting CLI, Tracebook:
validation, normalized duplicate detection, timezone filters, nearest-rank p95,
Unicode table output, atomic file output and a 10,000-record case. A separate
reviewer-created acceptance suite was outside the coding workspace. A provided
verifier copied each source snapshot to an isolated remote directory and ran
all project/acceptance tests there. Recorded model Bash calls were navigation
or the approved remote verifier; no project execution ran on the Mac.

| Phase | Logical model requests | Tool rounds | Result |
| --- | ---: | ---: | --- |
| Initial implementation, including a mid-turn steering message | 37 | 36 | Completed; own 17 tests and independent 62 checks passed after fixing the initial timestamp failure |
| Resume with the repaired TUI; add `summarize --top N` | 12 | 11 | Completed; own 20 tests and independent 67 checks passed |
| Manual compaction, then editor-submitted continuation | 2 | 1 | Completed; all 20 + 67 checks passed again, without code changes |

The first turn ran from `2026-10-11T02:12:23.766Z` to
`2026-10-11T02:29:06.486Z`, about 16 minutes 43 seconds. Development and remote
verification pauses between phases are not counted as continuous model work.
In total, the three completed turns recorded **51 logical model requests and
48 tool rounds**. These are Runtime counters, not a claim that the HTTP layer
made exactly that many attempts.

Observed turn usage totals (excluding the separately accounted compaction
utility): input 200272, visible output 16563, reasoning 33268, cache-read 1446912,
cache-write 0, provider-total 1697015 tokens. No monetary cost is inferred.

The real `/compact` command completed in one utility attempt: input 24658,
output 797, reasoning 917, cache-read/write 0, provider-total 26372. It covered
one completed loop / 81 items and retained 29 items. The Context view's local
estimate changed from approximately 207291 to 35485 tokens; these are explicitly
local estimates, not provider billing. The following turn succeeded with a new
shorter context and recorded no cache reads. This is an observation, not proof
of the gateway's cache-routing or retention implementation.

## Native interaction observations

- Multiline Chinese/emoji/combining-mark text was pasted into the real editor
  before Enter. Undo removed the draft, redo restored it, and the mid-turn
  submission displayed `Steering (accepted)` before being applied.
- Resume opened the saved tail, and the repaired TUI completed a subsequent
  coding turn using the same session.
- Tool fold clicks were injected through iTerm2 as terminal mouse-report input;
  the resumed Bash card's target stayed at visible row 15 when expanded.
  This is terminal-input-level evidence, not a claim of physical mouse testing.
- A controlled original-versus-fixed window-resize comparison used the same
  saved conversation. At 89 to 120 columns, the original build skipped the
  first visible fragment at one tested scroll position; the repaired build
  retained it at all four nonempty tested positions. One blank-row position
  was intentionally not counted as a text-preservation case.
- iTerm2's `contents` includes scrollback and a trailing newline. The final A/B
  capture explicitly removed the trailing delimiter and selected the actual
  last `rows` lines. Earlier exploratory captures without this normalization
  are not used as final evidence.
- The manual compaction status identified the operation separately from the
  already completed/persisted turn, returned to ready, and exposed its result
  through `/context`. A planned busy-submit check was skipped because the
  operation had already finished; it is not claimed as a native pass.

Screen capture was unavailable in this environment. These observations do not
establish pixel/color correctness, IME candidate-window behavior, native
clipboard selection, or an automated physical-pointer positioning test.

## Defects and repairs

1. **Resize reading anchor:** nearest row-start offsets could skip the text
   being read. A first repair based on rendered byte length was rejected by
   independent review because rendered Markdown and raw source offsets differ.
   The accepted repair chooses the preceding logical source boundary within a
   retained section, preserving exact/empty matches and unloaded fallback.
   Literal UTF-8 and parsed `**a** ... TAIL` reflow regressions cover this.
2. **Assistant-only historical tool folds:** cards rendered without a resident
   ToolResult owner were omitted from per-card/global fold state lookup, and
   pruning discarded their overrides. The repair uses renderer-compatible
   owner precedence and retains actual resident assistant ToolCall identities.
3. **Preview-only unread output:** accepted argument-preview growth was missing
   from the incoming-content stamp. The bounded preview marker now participates
   without hashing or cloning the accumulated body; stale updates remain ignored.
4. **Last visible inline tool:** the marker row was subtracted twice when
   selecting visible cards for hydration. Polling now uses the same raw
   transcript rectangle as rendering, leaving the marker adjustment in one place.

The first issue has native A/B evidence. The other three were independently
identified in code and reproduced through actual reducer/RPC/render regression
fixtures; they are not all represented as bugs observed with a physical mouse.
No new scheduler, protocol method, provider behavior or core-library change was
introduced.

## Remote regression results

All six bug-exposing regression cases failed on their respective original
production paths before repair. The final set contains seven focused tests,
including an additional source-boundary/fallback compatibility test.

| Gate | Result |
| --- | --- |
| Rust 1.85.0, all targets | 1387 passed, 0 failed, 59 ignored |
| Stable Rust, all targets | 1387 passed, 0 failed, 59 ignored |
| Current real Agent + mocked-provider E2E | 39 passed, 0 failed |
| Formatting | Passed after remote-only formatting |
| Stable strict Clippy and warning-denied rustdoc | Passed |
| Rust 1.85 strict Clippy | 8 pre-existing diagnostics in unchanged files; not declared passed |

The MSRV diagnostics are one `nonminimal_bool` in `app/tool_preview.rs` and
seven `format_collect` findings in `ui/tool.rs` / `ui/live_markdown_tests.rs`.
These paths were not modified by this repair; they were not suppressed or
silently included in its scope. No snapshot expected output was regenerated.

The final 2241-file source manifest was checked against the local working tree
with zero mismatches before adding this documentation. The repaired binaries
used for native follow-up were built from that same source. Independent final
source review approved the raw-Markdown anchor correction and the tool-state
changes. Documentation added afterwards does not alter the compiled inputs.

## Evidence and limits

The private experiment directory contains source manifests, RED/GREEN logs,
normalized native captures, the independent acceptance suite and verifier-run
records. The test workspace is `minicore-live-coding-20261011`, separate from
this repository. Full transcripts, API configuration and credentials are not
copied into the repository. A compact, secret-free resize A/B record accompanies
this report.

This run does not claim to have naturally reached the automatic compaction
threshold, produced a real upstream hard-overflow error, proved every provider
retry path, measured exact allocator or input-to-frame latency, or rerun hosted
CI. Manual compaction and successful post-compaction continuation were exercised
against the real service. Builds/tests remained remote, while the requested
native iTerm2 editor interaction ran on the Mac.
