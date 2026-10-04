# Performance Evidence

This document separates historical Stage A baselines from the current C2b/C2c
structural evidence. Timing values are workload measurements, not hard CI
limits and not terminal input-to-frame latency claims.

The current refactor package is **0.3.0**. The E1–E3 measurements below are
recorded remote-Linux evidence; they do not imply that the new hosted matrix or
native macOS/Windows execution has already run.

## Current Context Read Scheduling

`session.context` has no recurring timer or foreground/background cadence.
Existing events, operation responses, authoritative completion, known gaps and
Context open/F5/Refresh produce one-shot demand through the shared query slots.
Pending demand resumes on real slot/queue progress, and an active snapshot or
read error does not schedule another read. This is a scheduling change, not a
new throughput, RSS or interactive latency measurement. Historical measurements
and archived verification reports below remain evidence for their named sources.

## Current Remote F-Review Run

The current remote tree is source commit `9e399d9`, after remediation
`daa944a`; core baseline `0aa64c5e4d9211351123db059547beddb15c2cce` is historical
provenance. The current source manifest is 375 entries with SHA-256
`607a4b52d4b865b6210f8865473f3a8aa8126b15d374b604f050fc4ebb09ba00`. Rust
1.85.0 and stable Release performance each passed **9/9**:

```text
Rust 1.85 production App 256 KiB draft: edits=4096 p95_us=700 p99_us=755,
  draft_bytes=258048 retained_capacity_estimate=5402688 composer_full_joins_delta=0
Rust 1.85 same direct Composer workload: p95_us=192 p99_us=220,
  draft_bytes=258048 retained_capacity_estimate=5402688 composer_full_joins_delta=0
C2 stable layout: layout_calls_delta=0 history_bytes_cloned=0 viewport_rows=40000
Rust 1.85 C2c 120x40: p95_us=3571 p99_us=4367 durable_rows=43870,
  layout_calls=0 history_bytes_cloned=0 viewport_bytes=4396336
C2b worker: durable_rows=51101 deltas=1000,
  layout_calls=0 history_bytes_cloned=0 viewport_bytes=2986911
Rust stable production App 256 KiB draft: p95_us=662 p99_us=676;
Rust stable direct Composer: p95_us=238 p99_us=243;
Rust stable C2c 120x40: p95_us=3565 p99_us=4295.
real NativeClipboard helper: 2004 ms (Rust 1.85), 2004 ms (stable), direct PID
observed and reaped; run_commands/input/scroll/cancel kept progressing.
```

The 256 KiB P95 is local edit processing, not terminal input-to-frame latency.
The direct production draw counter recorded 10 draws during the interaction
probe and 2 draws during 30 seconds of idle on both toolchains. The idle probe
had zero stable history layout calls and zero historical body clones. Python
`resource` also
recorded process CPU and peak-RSS observations for the Linux PTY processes;
exact allocator/RSS accounting remains unclaimed.

The independent remote 0.2.8 archive at `9d11ee6` recorded P95 1492 µs and
P99 1907 µs for the same direct Composer workload, versus current Rust 1.85
values of 192 µs and 220 µs (stable: 238 µs and 243 µs). This is a
fixed-workload comparison, not a general
application speedup or terminal input-to-frame claim; archive details and the
hash are in [`verification/v03-f/README.md`](verification/v03-f/README.md).

## Excluded Local Execution Deviation

Phase F once violated the original remote-only Rust/Cargo requirement by
running locally. Its older six-workload values remain below only as excluded
provenance; they are not current acceptance evidence:

```text
local rustc 1.98.0: 6/6 older workloads; C2c p95_us=2503 p99_us=2880
```

## Prior Remote Final-Source Run

The pre-remediation remote final-F release set passed the older **6/6**
structural workloads. Those fixed-workload values remain historical context;
the current F-review values and the new PTY/baseline evidence are recorded in
[`verification/v03-f/README.md`](verification/v03-f/README.md).

## Environment

| Item | Value |
|---|---|
| Builder | `root@192.168.20.199` |
| Workspace | `/root/minicore-tui-v03-refactor/tui` |
| Authoritative toolchain | Rust 1.85.0 |
| Workload terminal | 120×40 release probe; content width 119 |
| History workload | 7300 synthetic assistant blocks, about 43,870 durable rows |
| Stream workload | 1000 real `output_delta` events |

## Historical Baseline

The Stage A release baseline used the synchronous full-transcript preparation
path. It measured roughly 51,101 rows and 118–121 ms per cache-miss build on
the recorded remote Linux host. `all_lines` materialized the complete row
vector and was a diagnostic helper, not a target production API.

Those numbers are retained for before/after context only. They are not used to
claim that the current frame path has the same cost.

## C2b Structural Evidence

Recorded C2b release probe:

```text
durable_rows=51101
deltas=1000
layout_calls=0
history_bytes_cloned=0
viewport_rows=40000
viewport_bytes=2986911
```

The probe installed the durable layout once, then applied 1000 real live
output events. Stable history was not rebuilt and the viewport materialized
only the measured visible window. A layout build is now owned by the single
serialized `LocalJobs` layout worker; production active sessions do not use a
synchronous durable-layout fallback.

## Historical C2c Release Evidence

The E3-era C2c release probe after the decode-worker and ToolFacts changes:

```text
p95_us=7610
p99_us=7932
durable_rows=43870
layout_calls=0
history_bytes_cloned=0
viewport_rows=40000
viewport_bytes=4396336
retained_layout_bytes_estimate=8035080
c2c_max_tree_vm_hwm_kib=47172
```

The samples are synthetic frame-processing measurements for the fixed 120×40
workload; reruns can vary with host scheduling. They are **not** terminal input
latency or terminal input-to-frame P95/P99 measurements. The historical focused
C2b/C2c probes passed; the current remote F-review Release suite is recorded
above. The source/perf workload is 1000 deltas,
7300 history blocks, 43,870 durable rows, a 119-column content width, and a
40-row viewport; these units must remain in future logs.

## Decode Worker Evidence

Complete automatic items at or below `MAX_AUTO_ITEM_BYTES` are assembled as a
bounded `Arc<str>` canonical body and submitted one at a time to the single
serialized decode worker. The App retains at most one active decode identity
per process plus the bounded encoded page owned by the current read chain.
Worker results carry `session_epoch`, `read_chain`, and either the history
index or exact `TurnRef`/turn-local index. Stale results release the worker
identity and cannot install into a newer view.

The deterministic tests cover:

- worker identity and typed Runtime-item decoding;
- App installation only after worker completion;
- stale epoch completion without installation;
- cancellation completion and queue shutdown ownership.

A fresh release decode-throughput/RSS measurement is **Not run**. No timing or
RSS claim is made from the unit tests; the exact allocator/RSS proof requested
by the broader specification is intentionally not claimed.

## Budgets

The implemented owner-level targets are centralized in `src/limits.rs`:

- history body: 32 MiB;
- layout cache, including in-flight partials: 48 MiB;
- live output: 4 MiB per loop and 16 MiB per session;
- tool presentation: 1 MiB per stream and 16 MiB per session;
- composer draft: 256 KiB and 8 MiB for retained submitted drafts;
- one automatic item decode: 8 MiB;
- remote read slots: 2 with bounded waiting/coalescing;
- decode and layout workers: one serialized owner each with bounded queues.

History eviction releases semantic owners and reopens the corresponding read
gaps. Layout eviction accounts both installed cache entries and in-flight
partials. These are retained-payload estimates, not exact process RSS or
allocator-capacity measurements.

## Historical E1 tool-detail regression (final implementation `7ca349b`)

Both Rust 1.85.0 and stable 1.97.1 passed the 762-test default suite and all
30 real-Agent E2Es on the same remote Linux host. The six ignored Release
workloads were rerun with Rust 1.85.0; see
[the complete E1 log](verification/v03-e1/e1-performance.log).

```text
c2b_worker: durable_rows=51101 deltas=1000 layout_calls=0
            history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=2986911
c2c_120x40: p95_us=7941 p99_us=8306 durable_rows=43870
            history_bytes_cloned=0 layout_calls=0 viewport_rows=40000
            viewport_bytes=4396336 retained_layout_bytes_estimate=8035080
```

The preceding D3 P95/P99 was 7610/7932 μs. These small timing differences are
not claimed as a speedup, and neither run measures terminal input-to-frame
latency. The diagnostic clone/cold-prepare helpers remain in the full log;
they are not the production asynchronous hot path.

E1 uses the existing serialized layout worker for tool text, with grapheme wrap
indexes and viewport-only row materialization. The detail layout text/index
capacity participates in the 48 MiB layout budget. One detail has four bounded
1 MiB stream windows (not an unbounded cache per visited tool); a matching
ToolFacts result body is shared by Arc. Stream-budget/head-eviction and Arc
reuse tests pass. Tiny process events coalesce into capacity-accounted 16 KiB
pages, with an independent 128-chunk bound; the 32,000-byte event regression
retains two pages and leaves shared in-flight snapshots unchanged. A fresh E1
peak-RSS or manual-terminal measurement is **Not
run**, not inferred from byte accounting.

## Historical E2 workspace regression (final implementation `d553e96`)

On each of remote Linux Rust 1.85.0 and stable 1.97.1, the complete default
suite passed **788 tests, 0 failed, 41 ignored**, and the separate fixed-Agent
serial run passed **32/32**, including all original 30. All six ignored Release
workloads passed; see [the final E2 log](verification/v03-e2/e2-performance.log).

```text
c2b_worker: durable_rows=51101 deltas=1000 layout_calls=0
            history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=2986911
c2c_120x40: p95_us=7500 p99_us=7951 durable_rows=43870
            history_bytes_cloned=0 layout_calls=0 viewport_rows=40000
            viewport_bytes=4396336 retained_layout_bytes_estimate=8035080
```

This does not claim an optimization over E1's 7941/8306 μs, or terminal latency.
Workspace query state adds no ordinary-editor buffer joins or stable-history
layout work. File preview's 512 KiB raw body uses small coalesced immutable
chunks; source indexing/sanitizing/wrapping stays on the same serialized layout
worker, with text/index capacity charged to the existing 48 MiB budget. Only
viewport rows are materialized. Candidate/match retention is capped at 500
records and 1 MiB. Tests exercise 32,000 tiny file chunks, a bounded pathological
grapheme, and a real 140 KB UTF-8 line over multiple same-line pages. Real-Agent
E2Es also use the owned file-layout worker. These are structural checks, not a
new file-preview throughput or peak-RSS measurement.

## Historical E3 read-only review regression (final implementation `733df37`)

On each remote Linux toolchain, Rust 1.85.0 and stable 1.97.1, the final default
suite passed **812 tests, 0 failed, 43 ignored**, and all **34/34** serial fixed-
Agent E2Es passed. The six ignored Release workloads also passed; see
[the final E3 log](verification/v03-e3/e3-performance.log).

```text
c2b_worker: durable_rows=51101 deltas=1000 layout_calls=0
            history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=2986911
c2c_120x40: p95_us=7999 p99_us=8335 durable_rows=43870
            history_bytes_cloned=0 layout_calls=0 viewport_rows=40000
            viewport_bytes=4396336 retained_layout_bytes_estimate=8035080
```

The unchanged structural probes still show zero stable-history layout calls
and body copies. These single-run synthetic timings vary with scheduling;
they are not an optimization claim over E2 or real terminal input latency.
Changes uses the same two read slots and serialized layout worker, with a
1 MiB diff body, 16,384 logical lines, 131,072 display rows, 500 records / 1 MiB
record accounting and the existing 48 MiB layout cache budget. Footer status
metadata has a separate global 1 MiB bound and is observed only explicitly.
An extreme-width layout test checks the display-row/capacity bound; a real
140 KB single-line diff checks raw fragment assembly through the worker.

At E3, Context reused B's query/operation owner and its recurring scheduling.
The retired-read regression checked that occupied slots did not cause timer
spin and that confirmation resumed on completion. Those historical checks do
not describe the current one-shot context scheduling above. The E3 Agent harness
honored the application's actual timer deadlines rather than starving a quiet
transport; no Provider deadline or assertion was relaxed.
There is no fresh E3 peak-RSS, allocation benchmark or interactive latency claim.

## Not run / Remaining

The phase-F query-slot fix is a lifecycle correctness change, not a throughput
change. Its focused regression covers scope invalidation of waiting, ready,
refresh, and detached follow-up state while preserving real in-flight
ownership. No performance number is inferred from that test.

- decode-throughput and RSS measurements for the current serialized decode
  worker;
- typed explicit decoding/read workflow for items over 8 MiB; oversized items
  remain bounded placeholders rather than fabricated complete bodies;
- exact allocation-capacity and RSS accounting, which is intentionally not
  attempted here;
- terminal input-to-frame latency under real interactive streaming;
- manual iTerm2/IME/clipboard validation, exact allocator/RSS accounting,
  terminal input-to-frame latency, and external-provider validation remain
  unrun. The exact Linux OS clipboard-helper cancellation and paused-Agent/
  large-stdout scenarios are covered by the current child-process probes; they
  do not imply native desktop or hosted CI behavior. E2 workspace and E3
  Changes/Context have the automated evidence above.
  D2 search/copy/export and D3 editor/settings measurements remain in
  `docs/refactor-acceptance.md`. Hosted CI and native macOS/Windows execution
  remain **Not run** until those jobs execute; Linux kernel-PTY validation
  passed on both toolchains.
