# Performance Baseline (Stage A)

This records the **measured** v0.2.8 baseline that the stage C performance work
must improve on. It is not a claim that the refactor passed a performance
budget; the stage C structural counters and timing runs are still pending.

## Environment

| Item | Value |
|---|---|
| Builder | remote Linux host `192.168.20.199` |
| Kernel / arch | Linux 6.12.94, x86_64, 24 cores |
| Toolchain | `cargo 1.97.1` / `rustc 1.97.1` (workspace MSRV is 1.85.0) |
| Workspace | `/root/minicore-tui-v03-refactor/tui` |
| Build | `cargo test --release --locked` |
| Date | 2026-09-18 |

The wall-clock numbers are machine-dependent and are **not** CI assertions. They
exist so the same fixture can be rerun for an apples-to-apples before/after.

## Structural baseline

Measured through public behavior (no hidden telemetry hook):

| Metric | 20 msgs × ~240 B | 200 msgs × ~240 B | Note |
|---|---:|---:|---|
| prepared transcript rows (`total_lines`) | 701 | 7001 | `tests/performance.rs::baseline_prepared_rows_scale_with_total_history` |
| owned rows from `all_lines` | — | 7001 | equals the full prepared row set; `baseline_all_lines_materializes_full_transcript` |

The current renderer materializes the entire durable transcript into one owned
`Vec<Line>` on every preparation. Stage C must make a viewport preparation
proportional to the viewport plus overscan, not the total history.

## Timing baseline (Release, ignored tests)

Command:

```bash
cargo test --release --locked --test performance -- --ignored --nocapture
```

| Probe | Result |
|---|---|
| `measure_all_lines_rebuild_latency` (1000 msgs, 20 rebuilds) | 7001 rows/call, **≈15.2 ms/call** |
| `measure_live_delta_rebuild_cost` (1000 msgs, one live delta) | 7001 → 7006 rows, delta 5 rows |

The single-delta probe shows the current live path still rebuilds the whole
row vector internally even though the visible delta is small. Stage C removes
the full-history clone and stale-block re-layout.

## Backpressure baseline

`tests/backpressure_baseline.rs` drives a real spawned child that never reads
stdin:

- 64 requests of 512 KiB fill the 64-slot outbound channel plus the OS pipe;
- the next `RpcProcess::send(...).await` does not complete within 300 ms.

This proves the current UI admission path (`send().await`) can block
`App::update`'s caller. Stage B replaces it with a synchronous `try_send`
(32-slot queue, 28 ordinary + 4 control) that returns immediately and keeps
the input.

## Not measured / not run

- No three-platform numbers; only the remote Linux builder was used.
- No resident-set (RSS) or allocation profiling; only row counts and wall
  clock.
- No terminal-input-to-frame P95/P99 latency under streaming; the stage C
  `PerfCounters` and the fixed-workstation latency runs are pending.
- No 256 KiB paste edit-latency measurement (stage C).
- No clipboard-hang responsiveness measurement (stage C).
- No upstream Agent/Runtime benchmarks were run; their sources were only read.

## Budget targets from the spec (not yet enforced)

These are the Spec §21/§25 budgets the refactor aims at. They are recorded here
as targets, not results: outbound line 1 MiB; outbound queue 32 (4 control);
inbound frame 32 MiB; inbound pending wire bytes 64 MiB; read queries 2;
deferred ≤16; composer 256 KiB; all drafts 8 MiB; unsent steer 8 items/256 KiB;
history body 32 MiB; single auto-decoded item 8 MiB; layout cache 48 MiB; tool
UI stream 1 MiB/stream, 16 MiB total; live display 4 MiB/loop, 16 MiB total;
log 200 lines × 4096 B; search hits 500.
