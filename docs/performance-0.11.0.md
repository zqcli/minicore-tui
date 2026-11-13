# TUI 0.11.0 performance and size

This release reduces native history installation work and temporary tool-output
allocation. It also consolidates catalog reads, separates automatic workspace
status reads from unrelated controls, and removes unused production APIs and
duplicate draft/input ownership. Host remains pinned to 0.11.0 at
`32f074c57868533af81b0c6491440bae24b872c8`.

## Measurement conditions

The baseline is TUI 0.10.0, commit
`03ba242177901f9a380317919f799f98a63df4ce`. Measurements used the same Linux
workspace and Rust 1.90.0. The repository's development profile uses optimization
level 1 for TUI code and level 2 for dependencies. Standalone comparison probes
used optimization level 1. No compilation ran concurrently with the history
measurements.

These are local diagnostic timings, not release-build service-level guarantees.
The machine is shared, and allocator/scheduling noise is visible in small
measurements. History probes exclude record construction, Host I/O, event delivery
and terminal drawing. The output-capacity probe measures retained `String`
capacity, not process RSS or complete native argument storage.

## Native history installation

The comparison probe installs uniquely positioned native User records with unique
Message IDs and text `review message {i}`. It warms up with 300 records, then
reports seven-sample medians for each size. Each sample creates an empty
Transcript, installs the batch, replays the same batch, then replays its last
60 records. Record construction is outside the timed sections. The new empty
Transcript plus `install_history_records` replaces the removed production
`from_history` convenience function without changing the operation being timed.

All values below are milliseconds.

| Records | Initial install 0.10.0 | Initial install 0.11.0 | Full replay 0.10.0 | Full replay 0.11.0 | Last 60 replay 0.10.0 | Last 60 replay 0.11.0 |
|---:|---:|---:|---:|---:|---:|---:|
| 1,000 | 1.986 | 0.342 | 5.160 | 0.125 | 0.382 | 0.107 |
| 5,000 | 63.614 | 7.575 | 206.089 | 5.272 | 3.198 | 2.562 |
| 10,000 | 268.704 | 7.478 | 1,327.014 | 4.885 | 9.002 | 3.725 |
| 20,000 | 1,423.118 | 11.095 | 6,265.797 | 11.162 | 23.212 | 6.715 |

At 20,000 records, initial installation is approximately 128 times faster and
full replay 561 times faster in this probe. Replaying the last 60 records is
approximately 3.5 times faster. The smaller gain for a short page is expected:
the temporary index still traverses the currently loaded transcript once.

The implementation builds one batch-local index for native positions, responses,
tools and terminal turns. It replaces repeated scans per incoming record with
indexed lookups. Unchanged replay preserves block revisions and skips sorting;
already ordered additions avoid a sorting allocation. There is no persistent
second transcript index to synchronize or retain between batches.

The checked-in `native_history_performance` test also exercises mixed User,
Assistant and ToolResult records, preserving canonical identity and revisions:

| Native records | Initial install | Full replay | Last 60 replay | 100,000 active response checks |
|---:|---:|---:|---:|---:|
| 1,000 | 6.095 ms | 0.354 ms | 0.252 ms | 0.903 ms |
| 5,000 | 22.497 ms | 3.586 ms | 2.584 ms | 1.086 ms |
| 10,000 | 39.002 ms | 8.162 ms | 8.620 ms | 1.174 ms |
| 20,000 | 107.978 ms | 27.746 ms | 17.565 ms | 0.959 ms |

These mixed-record results are single diagnostic runs, not the seven-sample
comparison above. Active response checks search backward for the matching native
key before testing whether it is canonical. A live response at the tail stops
the search immediately. An arbitrary missing or old key can still require a scan.

## Tool output and input

The output probe appends a single ASCII chunk to a fresh bash ToolCall, then
settles it. Both versions retain at most 65,536 bytes of displayed output, but
0.10.0 allocated the complete incoming chunk before truncating it.

| Incoming chunk | Retained capacity 0.10.0 | Retained capacity 0.11.0 |
|---:|---:|---:|
| 64 KiB | 64 KiB | 64 KiB |
| 1 MiB | 1 MiB | 64 KiB |
| 8 MiB | 8 MiB | 64 KiB |

Capacities above are unchanged by `settle()`. The 8 MiB case uses 128 times less
capacity for this output buffer. The code now chooses the UTF-8-safe retained
prefix before reserving and copying. Regression tests cover ASCII, CJK, emoji
and chunk boundaries.

Tool cards borrow bash commands, write content and patches from the complete
native arguments. Only edit's derived plus/minus display text needs separate
storage. Collapsed cards collect no input-line references; expanded cards collect
at most 200 while retaining an exact hidden-line count. Successful native read
text no longer pays for an unused pretty-printed JSON result first.

These changes do not truncate native arguments, cap all transcript memory at
64 KiB, or discard canonical history. Loaded history can still grow with the
number and size of retained records.

## Drawing checks

The existing two ignored drawing tests also pass. An isolated repeat, without
compilation or source counting alongside it, produced:

| Drawing fixture | 0.10.0 | 0.11.0 |
|---|---:|---:|
| First draw over 5,000 blocks | 163.151 ms | 112.856 ms |
| Steady draw p50 | 190 microseconds | 201 microseconds |
| Steady draw p95 | 475 microseconds | 548 microseconds |
| 50,618-byte stream, first five frames mean | 382 microseconds | 434 microseconds |
| Same stream, last five frames mean | 295 microseconds | 215 microseconds |

These draw-only fixture results vary between runs; no general renderer speedup
is claimed. They bypass native history installation and cannot validate the
history optimization. The many-card fixture retains the same output capacity:
117,736,000 bytes before settling and 59,272,890 afterward. Its direct fixture
construction does not exercise the single-chunk allocation fix.

## Code size

Counts cover all tracked Rust source for all platforms, excluding dependencies,
generated/build output, Markdown, lockfiles and text snapshots. Effective lines
exclude blank and comment-only lines; Rust attributes count as code. Dedicated
test files and inline `cfg(test)` items count as tests, including
`src/testing/production.rs` and the ignored performance fixtures.

| Effective Rust lines | 0.10.0 | 0.11.0 | Change |
|---|---:|---:|---:|
| Production | 17,851 | 17,744 | -107 |
| Tests | 11,389 | 11,878 | +489 |
| Total | 29,240 | 29,622 | +382 |

Production code is smaller; total Rust code grows because the new ownership,
identity, editor-error and allocation boundaries have regression coverage.
Historical design documents move into `docs/archive/`; this is organization,
not a production-code reduction. Removing the unused direct `sha2` dependency
does not remove Host's transitive dependency on it.

## Validation and reproduction

The local suite passed 472 tests. The three ignored performance tests were run
separately and passed. The isolated embedded-Host helper remains ignored for
direct invocation; the integration tests execute it as their production probe.
Formatting, Clippy with denied warnings, and Rustdoc with denied warnings pass.
Native OS checks and release packaging remain part of CI.

```bash
cargo fmt --all -- --check
cargo clippy --locked --offline --all-targets -- -D warnings
cargo test --locked --offline --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --locked --offline --no-deps
cargo test --locked --offline --lib native_history_performance -- --ignored --nocapture
cargo test --locked --offline --lib perf_tests -- --ignored --nocapture
```

Run timing probes without concurrent builds. These commands require the locked
dependencies to have been fetched. Cross-platform CI runs the normal tests;
the optional timing probes do not impose machine-dependent latency thresholds
on release acceptance.
