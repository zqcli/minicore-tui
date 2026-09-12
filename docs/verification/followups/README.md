# Tool, Reload And Native Subagent Acceptance

Accepted on September 12, 2026. Package versions remain **TUI 0.2.8 / Agent
0.3.3**. Runtime remains **0.4.1** at
`6cd2bdbc634437dea925495c61c7eb0be10ba171`; no Runtime source, dependency pin,
release tag, push, or new hosted-CI result belongs to this batch.

This is the current follow-up record. The previous
[Session-management acceptance](../session-management/README.md) and its
provenance remain valid historical records, not current artifact manifests.

## Source Boundaries

The parent owns verification, staging and commits. Implementation and independent
review used the existing `cus-resp/gpt-5.6-luna:max` sessions. Independently accepted
tasks were committed separately:

| Repository | Commit | Scope |
|---|---|---|
| Agent / TUI | `fbae60f` / `8010a82` | Safe bounded Tool failure body, fold hint and live/history projection |
| TUI | `99f79ff` | Monotonic 100 ms Working animation, independent of RPC/Tick frequency |
| TUI | `80ca414` | One transparent spacer between consecutive User cards; shared copy/hit geometry |
| TUI | `80b03f3` | Startup guidance for new drafts and confirmed empty idle Sessions |
| Agent | `f5abe83` | Bounded Codex single-file Update File compatibility alongside unified patches |
| Agent / TUI | `f08984b` / `30ea7ca` | Source-aware configuration reload and staged read-only reconciliation |
| Agent | `f1697f7` | Native bounded stateless subagent single, parallel tasks and chain |

The delivery was rebuilt from fresh Git archives, not from mutable worktrees:

- TUI: `30ea7ca828f59cab895486beebc98e4e4f4ecf2e`.
- Agent: `f1697f78ce48c8f5f3fde0dc9903c153022bfd9e`.
- Exact archive, executable and native-result hashes are in
  [provenance.json](provenance.json). Later documentation-only commits do not
  change the accepted Rust sources.

## Gates

All accepted Rust compilation, testing, formatting and linting ran on
`root@192.168.20.199`. macOS executed and inspected downloaded binaries only.
The earlier unauthorized local-Rust work is explicitly excluded below.

| Gate | Result | Evidence |
|---|---|---|
| TUI stable and Rust 1.85 all-targets, each | 498 passed / 0 failed / 19 ignored | `logs/accepted/minicore-tui-{stable,msrv}.log` |
| Agent stable and Rust 1.85 all-targets, each | 369 passed / 0 failed / 2 ignored | `logs/accepted/minicore-agent-{stable,msrv}.log` |
| Real-Agent loopback E2E, stable and MSRV, each | 18 passed | `logs/accepted/e2e-{stable,msrv}.log` |
| Stable fmt, strict Clippy, warning-denied rustdoc | PASS for both projects | `logs/accepted/` |
| macOS Debug and Release, both projects | Rust 1.98.0 / Clang-LLD 19; Mach-O/signatures PASS | `logs/accepted/`, `scripts/accepted-build.sh` |
| Debug executable/dSYM UUID pairing | PASS for both projects | `installation.log` |
| Real iTerm2/PTTY normal and panic | Exit 0 / 101; unchanged stty; no unflushed marker | `native/tty/` |

Counts are sums across all Cargo test binaries. Agent's 343 library tests alone
are not its 369-test all-target total. The ignored E2E suite is executed separately;
it is not counted as behavioral coverage by the default zero-test invocation.
Existing MSRV strict-Clippy diagnostics remain historical limitations, not fixed
or suppressed by blanket allowances. There is no new Windows/hosted-CI claim.

## Native Evidence

Final runs use actual iTerm2 screen text/input, actual TUI/Agent executables,
fresh absolute temporary stores/workspaces, and loopback-only Responses servers.
No user configuration or Store was used by these scenarios.

- `native/followups-{debug,release}/`: each made 19 actual HTTP requests. Checks
  cover startup/new-draft guidance, Esc cancellation without retry, live failure
  body and real mouse folding, Codex patch target plus whole workspace
  content/entry-set comparison, all three stateless subagent modes, strict tool
  schema, child tool isolation, inherited prompt/reasoning, actual outputs/usage,
  chain substitution, and no child Store Sessions. Idle reload leaves history
  bytes unchanged and sends later requests to the new raw model/model alias.
- `native/session-{debug,release}/`: prompt-file snapshots, busy rename, filtered
  Session identity, non-active close, 60x16 default-Cancel deletion, survivor
  safety, and shared Model/Reasoning/Profile/New/Help/Logs panels.
- `native/stream-{debug,release}/`: reasoning part boundaries, Tool folding,
  receipt-paced Steer FIFO, Footer settings, long-history wheel scrolling,
  body-following scrollbar drag, and owned-process CPU observations.

Working glyph interval medians were **97/109 ms** in Debug and **100/94 ms** in
Release for held/flooded requests. Each approximately two-second window observed
20 glyph changes. These are screen samples, not render FPS. The old >20 changes
threshold assumed the superseded 33 ms animation; the harness now checks the
100 ms contract, while exact cadence is pinned by unit tests. The render cap and
selection cadence remain independent. Drag showed 10/8 distinct body windows in
12 samples. Process-CPU records retain every sample and the approximately 0.01 s
`ps` resolution; no matched-baseline performance improvement is claimed.

Busy reload is covered by automated state/order tests, not a dedicated native
busy-reload scenario. Consecutive User geometry is covered by reducer/render
regressions; native pixel parity is not claimed.

## Failures And Corrections

`logs/revisions/` retains failed builds and regressions separately from final
accepted logs. In particular, missing test imports and the initial missing match
arrow are compilation/setup failures, not behavioral REDs. Recorded behavioral
REDs cover reload History/state ordering, idle-event authority bypass, dropped
close verification, and late TurnRef binding. Review also found and repaired
terminal FIFO handoff, unknown Usage aggregation, short-output truncation, and
child interaction detection through reliable Runtime state watching.

`native/failures/completion-marker-offscreen/` is a **harness failure**: the patch
succeeded and the provider received its result, but the completion marker was
below the mouse-anchored viewport. Functional acceptance now explicitly follows
new output; independent scrolling assertions remain intact. The raw FAIL and
owned-window cleanup are retained. Its former `scroll-harness-failure` filename
was replaced to avoid implying a scrolling-product regression.

An implementer violated the remote-only rule during the earlier apply-patch
stage on September 11, 2026, executing local Cargo/Rust commands and overwriting
Agent Debug/Release. The incident was disclosed, local results were excluded,
and previously accepted remote binaries were restored before this delivery.
The audit is `audit/local-execution-violations.tsv`; rejected executables remain
in Agent `target/preserved-unauthorized-local-AdCugX/`. Logs named
`apply-patch-final-*` from those local runs are **not remote acceptance evidence**.
Subsequent implementation was source-only; the parent executed the accepted
Rust work on the authorized builder. No claim of an entirely violation-free
workflow is made. The final reviewer also reported that checksum validation
briefly created and removed two owned temporary output files; no repository,
Git, source or user artifact was changed. Its audit was source-read-only, not
literally write-free.

## Installation And Evidence Layout

All four accepted binaries and both Debug symbol bundles were staged first.
Each old executable was hard-linked into
`target/preserved-before-followups-AdCugX/` in its own repository; old inode and
hash were checked after atomic per-file replacement. Old symbols were preserved.
Installed paths are each repository's `target/{debug,release}/<project>` and
byte-match the accepted artifacts. See [installation.log](installation.log) and
[install-accepted.sh](install-accepted.sh). Installation did not edit user
configuration/Store data or restart user processes; existing processes may still
execute older images. The four replacements are not one filesystem transaction.

The pre-delivery cleanup removed only two regenerable incremental directories
(5,163,692,901 logical bytes) under three explicit cache roots, verifying 1,542
retained artifact hashes before and after cleanup. `cleanup.json` records this
operation; `cleanup-initial.json` records the earlier, separate cleanup. Unrelated
Runtime build work and its running process were excluded.

`FILES.sha256` verifies the tracked evidence files. `hashes.txt` addresses artifacts
and source tar files in the local accepted stash
`/tmp/minicore-followups.AdCugX/accepted/`; those large files are not duplicated in
Git. The stash includes the current manifest, archives, executables and native
evidence. Final followup harnesses import the bundled helper extracted from the
accepted TUI archive. Earlier Session/Stream one-off scripts retain original
absolute paths; their checkout helper was byte-compared with the bundled copy.
Complete evidence inputs are retained, but a portable harness or bundled SDK is
not claimed. No credentials, user Store, or startup configuration is archived.

## Scope And Use

`/reload` replaces the public `/refresh`; exact-turn wait reconciliation remains
internal. Agent ACK and TUI refresh completion are distinct. Failure or uncertain
delivery does not authorize automatic resend. A nonresponding Agent can leave the
reload barrier in place; no new generic reload-timeout/retry mechanism is claimed.
`/reload` reloads configuration, not running executable code. Existing Sessions
retain stored prompt, tool and approval snapshots; new Sessions use new profiles.

To enable native delegation, add `subagent` to the intended profile's existing
`tools` list, reload the configuration, and create a new Session. Approval policy
still applies. Stage 1 supports configured models/reasoning overrides, single,
up to eight tasks/chain stages, at most four parallel children, bounded output,
parent-workspace containment, and no recursive delegation. Ordinary completion
joins children before returning; external Tool/turn cancellation may defer join
to the owning Session/Agent drain while retaining every handle.

**Not implemented:** persistent alias/target, adopted/fork/exclusive Sessions,
steer/followUp control queues, a subagent manager panel, or compaction. These need
a separate durable ownership/link contract and are not implied by stateless
parity. The patch tool supports one existing file's unified/Codex Update File
patches, not full Codex Add/Delete/Move or all Git preambles. The original user
failure payload was not read; synthetic compatibility evidence does not establish
its unique original cause. Real upstream/TLS, macOS 11 hardware and pixel parity
also remain unverified.
