# Performance Evidence

This document separates historical Stage A baselines from the current C2b/C2c
structural evidence. Timing values are workload measurements, not hard CI
limits and not terminal input-to-frame latency claims.

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

## C2c Release Evidence

Current-tree C2c release probe after the decode-worker and ToolFacts changes:

```text
p95_us=7497
p99_us=7931
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
latency or terminal input-to-frame P95/P99 measurements. The focused current-tree C2b/C2c probes
passed; the full six-test ignored release suite also passed in this validation
cycle. The source/perf workload is 1000 deltas,
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

## Not run / Remaining

- decode-throughput and RSS measurements for the current serialized decode
  worker;
- typed explicit decoding/read workflow for items over 8 MiB; oversized items
  remain bounded placeholders rather than fabricated complete bodies;
- exact allocation-capacity and RSS accounting, which is intentionally not
  attempted here;
- terminal input-to-frame latency under real interactive streaming;
- D/E search, export, workspace, and external-editor workflows.
