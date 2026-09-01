# Delivery Verification

Verification date: 2026-09-01 (UTC+08:00)

This is the Phase 7 delivery record for the commit whose message is
`docs: document minicore-tui usage and backend limits`. Rust validation was
run on an isolated Linux builder with Rust `1.85.0`; no Cargo, rustc, fmt,
clippy, doc, or tree command was run on the developer machine.

## Pins

- Project baseline before this delivery: `0a8e048`.
- Agent repository: `https://github.com/zqcli/minicore-agent`, branch `dev`.
- Agent commit: `6d5e963031159c458212a92c690e515a2ac3761b`.
- RPC version: `0.2.0`.
- Contract document blob: `b8f4d57c6931cad8b99b39fdda0647a2539824a6`.
- Rust: `1.85.0`.
- Package: one `minicore-tui` Cargo package, Edition 2024.

## Commands And Results

All commands below passed on the isolated Linux builder. The delivery
wrapper used a 20-second SSH connection timeout; fmt/metadata/tree commands
were limited to 120 seconds; test/clippy/doc commands were limited to 900
seconds; the E2E wrapper was limited to 300 seconds.

```text
cargo +1.85.0 fmt --all -- --check                         PASS
cargo +1.85.0 test --locked --all-targets                    PASS
cargo +1.85.0 clippy --locked --all-targets -- -D warnings  PASS
RUSTDOCFLAGS=-D warnings cargo +1.85.0 doc --locked --no-deps PASS
cargo +1.85.0 tree -p crossterm                              PASS
cargo +1.85.0 metadata --locked --no-deps --format-version 1  PASS
cargo +1.85.0 clippy --target x86_64-pc-windows-gnu \
  --all-targets -- -D warnings                              PASS
cargo +1.85.0 test --target x86_64-pc-windows-gnu \
  --locked --all-targets --no-run                           PASS
```

The final default all-targets run reported `308` passed, zero failed, and
`2` ignored. The ignored entries are the real-Agent E2E and
real-PTY terminal test. The harness-free `agent_process` target is not a libtest target and is
excluded from this arithmetic; no compile target is counted as a test.

The exact libtest arithmetic was:

```text
lib 266 + main 6 + agent_e2e 3 + app_flow 10 + protocol 12
+ render_snapshots 4 + rpc_io 2 + terminal_restore 5 = 308 passed
agent_e2e ignored 1 + terminal_restore ignored 1 = 2 ignored
```

The metadata target set was:

```text
agent_e2e, agent_process, app_flow, minicore-tui, minicore_tui,
protocol, render_snapshots, rpc_io, terminal_restore
```

`cargo tree -p crossterm` reported one `crossterm v0.28.1`. The committed
snapshot set contains 27 text snapshots; the in-crate snapshot comparison and
four representative integration snapshot tests passed with no drift.

## Cache And Markdown Evidence

The cache evidence is in `src/ui/transcript.rs`:

- one initial durable preparation parses one pending Markdown user card;
- repeated `total_lines` and render calls do not increase the thread-local
  `cfg(test)` parse counter;
- width, theme, reasoning visibility, all-tools expansion, individual tool
  expansion, and durable block mutations invalidate or change the key;
- live output deltas leave the durable revision unchanged;
- stale prepared results are rejected;
- separate sessions retain separate prepared caches.

`src/markdown.rs` verifies that adjacent equal-style characters coalesce into
one Span while bold/style boundaries, CJK width, emoji, combining marks, and
line text remain unchanged.

## Real-Agent E2E

The ignored `real_agent_multi_turn_flow` test passed against the pinned Agent
and a loopback mock. It verified isolated configuration/workspace handling,
bootstrap, two turns, durable user/assistant reconciliation, pending wait
registration, transcript completion, shutdown response, and successful child
exit. The trap reported `E2E_CLEANUP_OK`; a follow-up remote audit found no
mock/Agent process and no E2E temporary directory.

No provider endpoint, API key, password, IP address, or private workspace path
is recorded in this document.

## CI And Platform Honesty

The GitHub Actions workflow is present at `.github/workflows/ci.yml` and still
defines Ubuntu quality, locked Linux/macOS/Windows tests, MSRV 1.85 tests, and
the single-Crossterm check. GitHub Actions was not run for this delivery and
there is no remote macOS runner evidence. Windows evidence is the GNU
cross-target clippy and no-run compile check above, not a claim that Windows
executed the suite.

The full non-PASS and NOT-RUN accounting is maintained in
[acceptance.md](acceptance.md); known runtime limitations are in
[known-limitations.md](known-limitations.md).
