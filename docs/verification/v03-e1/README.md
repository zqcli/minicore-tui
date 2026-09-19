# v0.3 E1 — tool detail evidence

## Scope and identity

- Start: `refactor/v0.3-full-project@740a528`, TUI working tree and index clean.
- Stream/DTO slice: `bd92e84` (Rust 1.85 fmt, 742 passed / 0 failed /
  37 ignored, Clippy `-D warnings`; `e1-stream-*.log`).
- Main detail implementation: `6653205ca3bd7d40a53a205c186e7bd38fbfb287`.
- Capacity/harness hardening: `e3f4af0cea068a49e763e5df2511c099f69f3f77`.
- Final implementation under validation: `7ca349b2ac8472079e0dc4dd07188fbbef62b2f3`
  (visible-snapshot copy and input-tab coherence after preview eviction).
- Agent: `061743369459299e66be97bf97d2b27352a39914`, version 0.5.0 / Protocol 1.
- Runtime: `6cd2bdbc634437dea925495c61c7eb0be10ba171`, version 0.4.1.
- Remote backend source/Cargo files matched the fixed local checkouts with
  checksum-only, read-only rsync comparison. The remote snapshots do not have
  `.git`; no remote Git HEAD is claimed. Local backend untracked spec files
  were left alone. No backend source, Runtime, Store or user configuration was
  changed, and no PAT or push was used.

Only E1 is implemented here. Workspace/file/changes/context main-area pages
(E2 onward), final three-platform acceptance, and parent review are not implied.

## Authoritative environment and commands

All compilation, formatting, testing, Clippy, rustdoc and performance execution
was remote on `root@192.168.20.199` in
`/root/minicore-tui-v03-refactor/tui`. No local Cargo compilation was performed.

- Linux `6.12.94+deb13-amd64`, x86_64; 24 logical CPUs; 31940 MiB RAM.
- Rust `1.85.0 (4d91de4e4 2025-02-17)` and stable
  `1.97.1 (8bab26f4f 2026-07-14)`.
- The requested fixedagent target was absent. The existing pinned Agent source
  was built **offline and locked**, without edits, using Rust 1.85 into
  `/root/minicore-tui-v03-refactor/fixedagent-target`.
- Agent binary SHA-256:
  `661b32976ad6ae2fbe2b33411c7d0d082f9782da70745e4e4a6602c87fb7b273`.
- `df -h /root` ran before build phases. Only this TUI's exact
  `target/debug/incremental` directory was removed to reclaim approximately
  8.4 GiB. The final run retained about 41 GiB free; sources/stores/configs
  were not removed.

`e1-verify.sh` records the complete credential-free command sequence. For each
of the two toolchains it runs:

```text
cargo +<toolchain> fmt --all -- --check
cargo +<toolchain> test --locked --all-targets --no-fail-fast
cargo +<toolchain> clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo +<toolchain> doc --locked --no-deps
MINICORE_AGENT_BIN=<fixedagent-target/debug/minicore-agent> \
  cargo +<toolchain> test --locked --test agent_e2e -- --ignored --test-threads=1
```

Then it runs the six ignored Release performance workloads on Rust 1.85 and
`cargo tree --locked -d`. The only new dependency is pinned `base64 = 0.22.1`;
existing duplicate transitive versions were inspected, not blanket-rejected.

## Final results

| Check | Rust 1.85.0 | Stable 1.97.1 |
|---|---:|---:|
| fmt check | Passed | Passed |
| All-target tests | **762 passed, 0 failed, 39 ignored** | **762 passed, 0 failed, 39 ignored** |
| Clippy, warnings denied | Passed | Passed |
| rustdoc, warnings denied | Passed | Passed |
| Real Agent E2E (including all original 28) | **30 passed, 0 failed** | **30 passed, 0 failed** |

The 39 default ignores include 30 E2Es and six performance tests, all executed
separately above. The remaining three are the manual matched-scrollbar benchmark
and two real-PTY terminal lifecycle tests; they remain **Not run**. Empty fmt
logs are expected on successful `--check` runs.

The first development E2E attempt exposed command facts arriving before an
execution/read record: these now have their own field in the existing ToolFacts
owner rather than being discarded. The test waits for the real process fact,
not merely the invocation/policy boundary. Stable Clippy also flagged the new
layout-work enum size; the conversation request was boxed, and **both complete
validation sequences were rerun**. No failing assertion was weakened to hide a
product failure.

Final capacity inspection additionally reproduced the tiny-event metadata
problem (`e1-tiny-chunks-red.log`): 32,000 one-byte events made 32,000 retained
chunks. The fix coalesces them into two 16 KiB pages, charges owned Vec capacity,
independently caps chunks at 128, and preserves in-flight layout snapshots. The
new regression passes in both final 762-test runs.

One full E2E rerun then reported 29 passed / 1 failed in the existing full-search
and export scenario, with the explicit live section absent. Inspection found
the harness could wait 10 seconds for RPC while deferring local export-job
completion; the model's gated request has a 30-second deadline. The inferred
race is documented in `e1-export-timing-failure-summary.txt`. Polling owned
local-job results promptly (as production's select loop already does) fixed the
scenario: `e1-export-timing-green.log` records its 0.91-second run, with all
assertions and the Provider deadline unchanged. Both complete toolchain checks,
all 30 E2Es, and the Release suite were rerun after this fix. Final logs are
those successful reruns, not the earlier attempts.

## Behavior evidence

- `src/app/panels_tests.rs`: 15 deterministic tests, including A-close/B-open,
  same-key tab changes, deferred capacity, deletion retaining actual in-flight
  slots, monotonic/authoritative facts, process-before-read, resource exhaustion,
  500/250 ms scheduling, terminal drain/empty EOF, draft/F6/Esc, scrolling, title
  hits, visible-snapshot copy during continuing output, input tabs surviving
  preview eviction, and 60×16 / 80×24 / 120×40 safe detail rendering.
- `src/rpc.rs::tool_detail_late_response_through_fragmented_transport_releases_only_its_slot`:
  7-byte fragmented NDJSON, same call ID in distinct loops, late A response
  through the real stdout reader, no B pollution or premature slot release.
- `tests/tool_streams.rs`: 12 tests for pinned DTO fixtures, base64/raw cursor,
  UTF-8 cross-page tails, invalid/control bytes, true empty EOF, empty retained
  gap, event/query overlap and recoverable gaps, stale EOF, bounded head
  eviction, tiny-event coalescing/capacity and immutable snapshots, content-free
  Debug, and reuse of the ToolFacts result Arc.
- `e2e_tool_detail_drains_real_nonpty_stdout_and_stderr_after_terminal`:
  real Bash `test -t 1` reports PIPE, 8000 lines of `中🙂` drain over multiple
  pages, stderr remains separate, and exit 7 does not become an RPC failure.
- `e2e_tool_detail_close_does_not_cancel_then_exact_turn_cancel_drains`:
  real running Bash survives detail close; the explicit cancel uses its exact
  session/loop identity; reopening drains retained output and reports the real
  cancelled/termination-confirmed command facts.
- Twelve conversation snapshot files change only by the separate `[详情]`
  title hit. No Editor/Footer/Rail row geometry changed. Existing card-fold,
  Unicode, source-copy and scrollbar regression suites still pass.

The details use the existing two-slot RPC owner and serialized layout worker.
No hidden-card output polling, second RPC owner, generic panel framework,
synthetic changes tab, full PTY or backend/Store migration was introduced.

## Release structure and timing

`e1-performance.log` records six passed workloads. The hot-path measurements are:

```text
c2b_worker: durable_rows=51101 deltas=1000 layout_calls=0
            history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=2986911
c2c_120x40: p95_us=7941 p99_us=8306 durable_rows=43870
            history_bytes_cloned=0 layout_calls=0 viewport_rows=40000
            viewport_bytes=4396336 retained_layout_bytes_estimate=8035080
```

The historical D3 synthetic values were P95 7610 μs / P99 7932 μs. This small
variation is not claimed as a speedup. The diagnostic full-materialization and
cold-prepare helpers are also retained in the log; they are not the asynchronous
production hot path. These are **synthetic frame-processing measurements**, not
terminal input-to-frame latency, a manual TUI test, or an E1 RSS measurement.

## Final tracked build-source equality

`e1-source.sha256` covers all **338 tracked files** under `src/`, `tests/`,
`snapshots/`, `.cargo/`, plus `Cargo.toml` and `Cargo.lock`. This includes the new
modules, all protocol fixtures, all E2Es and all changed snapshots. Documentation
and these evidence logs are intentionally outside the build-source manifest.

Manifest SHA-256:

```text
3869257bc4b38aa61744c9bd85cbd490648dd4bb694d11f661aa7fe4025e3514
```

The local manifest was checked remotely using `sha256sum -c --quiet` against
the tree that produced the final logs: all entries matched. `/tmp/mctui-*`
credential-bearing helpers were neither printed nor committed. This evidence
commit changes documentation only, not the validated build sources.

## Still Not run / handoff

- Parent/independent review (to be scheduled by the parent agent).
- Manual iTerm2/IME/clipboard/external-editor/tool-detail terminal interaction.
- macOS and Windows test execution; real Provider smoke testing.
- End-to-end terminal input latency and a fresh E1 peak-RSS measurement.
- E2 workspace/files/changes/context pages and full-project v0.3 completion.
