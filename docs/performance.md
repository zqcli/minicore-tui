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

The wall-clock numbers are machine-dependent and are **not** CI assertions.
They exist so the same fixture can be rerun for an apples-to-apples
before/after. The raw run logs are kept on the builder at
`/root/minicore-tui-v03-refactor/perf-baseline.log`.

## Two distinct paths

The baseline measures two different code paths deliberately:

- **production frame path** — `main::prepare_frame` calls
  `ui::transcript::prepare_conversation` once per changed frame and installs the
  result via `AppEvent::ConversationPrepared`. This is what a live frame and a
  new history page cost.
- **diagnostic helper** — `ui::transcript::all_lines` clones the already
  prepared rows. This is a test/measurement helper, **not** the per-frame cost,
  and is never cited as the production frame cost.

## Structural baseline

Measured through public behavior (no hidden telemetry hook):

| Metric | Value | Test |
|---|---:|---|
| prepared rows, 20 msgs × ~240 B | 701 | `baseline_prepared_rows_scale_with_total_history` |
| prepared rows, 200 msgs × ~240 B | 7001 | same |
| owned rows from `all_lines`, 200 msgs | 7001 | `baseline_all_lines_materializes_full_transcript` |

`prepared rows` scale with the number of messages: the renderer materializes
the entire durable transcript into one owned `Vec<Line>` on every preparation.
Stage C must make a viewport preparation proportional to the viewport plus
overscan, not the total history.

## Timing baseline (Release, ignored tests)

Command:

```bash
cargo test --release --locked --test performance -- --ignored --nocapture
```

| Probe | Result |
|---|---|
| `measure_prepare_frame_path_over_50k_rows` (7300 msgs, 5 calls) | **51,101 rows, ≈119.6 ms/call** on the production frame path |
| `measure_live_delta_rebuild_cost` (7300 msgs, 1000 real `output_delta`, 51,101 rows) | history rows 51,101 → 51,234 (+133 visible), live-push loop ≈11.3 ms total |
| `measure_all_lines_clone_latency` (1000 msgs, diagnostic helper, 20 clones) | 7001 rows/call, ≈17.6 ms/call |

The live-delta probe pushes 1000 real `output_delta` events into an active
loop and shows the visible delta is small (+133 rows) while the underlying
preparation still rebuilds the full history on each durable-revision change.
Stage C removes the full-history clone and stale-block re-layout.

### What the numbers do and do not cover

- **Covered:** production-path preparation time and materialized row counts at
  50k+ rows; a 1000-delta live update; the diagnostic clone helper.
- **Not covered:** cloned *bytes* and layout-call *counts* through a dedicated
  counter — the stage C `PerfCounters` will add those and re-measure. Stage A
  did not measure them and does not claim them.
- The 1000-delta probe appends to a live loop held in one process; it is a
  structural probe, not the Spec §25.2 fixed-workstation P95/P99 stream test.

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
