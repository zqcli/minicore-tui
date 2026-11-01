# AGENTS.md

Guidance for coding agents working in this repository. Humans should start with
[README.md](README.md). The source and [docs/architecture.md](docs/architecture.md)
define behavior; where this file and the code disagree, the code wins and this
file gets fixed.

## What This Project Is

`minicore-tui` is a fullscreen terminal frontend (Rust, ratatui + crossterm) for
[minicore-agent](https://github.com/zqcli/minicore-agent). The Agent is built
into the same executable: the TUI starts itself as `minicore-tui agent` in one
child process and talks to it over stdio NDJSON JSON-RPC (Protocol v2). The RPC
contract is the only interface; the only Agent Rust API used is
`minicore_agent::cli::run` for that subcommand. The TUI never reads the Agent
store or the workspace, runs no tool, calls no provider and holds no credential.
The one exception is remote mode ([docs/design-remote.md](docs/design-remote.md)):
it lists host directories under the configured roots and creates folders for the
phone's directory picker, and never reads file contents. Its look
follows pi's rail UI: rails, tinted cards, a dock with the Working line, the
editor and a footer.

It is built for long, large sessions: frame cost follows the changed blocks, not
the history length, an idle TUI uses no CPU, and memory stays flat over hours.

A change that belongs to the Agent (wire contract, storage, tools, compaction)
or the Runtime (loop semantics) is made there, not worked around here. Change a
backend repository only after confirming that is where the behavior belongs.

## Repository Map

| Path | Responsibility |
|---|---|
| `src/main.rs` | The `agent` subcommand, startup, Agent spawn, terminal guard, the `select!` main loop, frame pacing |
| `src/config/` | The TUI directory: `config.toml`, `models.toml`, prompts, scaffolding, the composed Agent config |
| `src/rpc.rs`, `src/protocol/` | Agent child and JSON-RPC client; serde DTOs and constants only |
| `src/app/` | The reducer: all mutable state, keys (`keys/`), mouse, selection, composer, overlays, async tasks |
| `src/app/resident.rs`, `live.rs` | Resident session collection and terminal navigation; operations carry explicit epochs |
| `src/app/operation.rs` | Typed mutations and receipt validation shared by terminal and phone adapters |
| `src/app/control.rs`, `recovery.rs`, `read.rs` | Snapshot-only control writes, bounded completion discovery, verified paginated reads and body installation |
| `src/app/local_files.rs` | Serialized, coalesced local writes and reload composition off the reducer |
| `src/remote/` | Remote mode: listener, web client, password sign-in, the phone protocol, the projection and host directories |
| `src/transcript/` | Blocks with revisions, tool calls, history mapping, the per-block layout cache |
| `src/ui/` | Drawing from `&App`: transcript, blocks, rails, markdown, dock, sheets, views, theme |
| `src/terminal.rs`, `src/terminal/` | Terminal guard and frame drawing; Windows paste decoding (`input.rs`) and frame batching (`windows_frames.rs`) |
| `src/testing.rs` | `FakeAgent`: a scripted in-process Agent behind a real client (tests only) |
| `src/app/tests/` | Reducer tests through `FakeAgent` with paused time |
| `src/ui/snapshots.rs`, `tests/snapshots/` | `TestBackend` frame tests and checked-in frames |
| `src/ui/perf_tests.rs` | Frame-cost benches (`#[ignore]`) |
| `tests/local_timezone.rs`, `tests/built_in_agent.rs` | Integration tests: the production local-time path; the built-in Agent serving a composed config |
| `docs/` | `architecture.md` (modules and invariants), `configuration.md`, `keybindings.md`; `design-v0.4.md` is historical |

Dependency direction: `protocol` ← `rpc` ← `transcript` ← `app` ← `ui`. `ui`
keeps no state; `ui::draw` writes only the layout cache, the selection reset and
the editor hit area.

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo clippy --locked --all-targets --target x86_64-pc-windows-gnu -- -D warnings   # Windows-only code
UPDATE_SNAPSHOTS=1 cargo test --locked --lib ui::snapshots                  # rewrite tests/snapshots/*.txt
cargo test --locked --lib perf_tests -- --ignored --nocapture               # frame-cost benches
```

Always pass `--locked`. Updating dependencies is its own commit: run
`cargo update`, then the full checks. `MINICORE_TUI_PERF_STATS=1`
prints draw counts and timings on exit of a live run.

## Code Rules

- Rust edition 2024 on current stable; no minimum version is declared or
  tested. `unsafe` is forbidden.
- Dependencies use plain version requirements (`"1"`, `"0.30"`), never `=`
  pins; `Cargo.lock` and `--locked` make builds reproducible.
- Write the least code that solves the problem. No speculative features,
  configuration, abstractions with one implementation, or state machines for
  cases that cannot happen. Reuse the patterns and helpers already here.
- No defensive programming: validate once at the boundary that owns the input
  (CLI, config files, Agent responses) and trust types and invariants after
  that. Do not clamp, re-check or `unwrap_or` around values that cannot be out
  of range; a clamp can hide a real bug.
- Delete dead code, unused parameters, always-true flags and test-only wrappers
  in production code. Do not leave compatibility shims for removed behavior.
- Comments:
  - `///` is one sentence, and only where the name and signature leave
    something unsaid. `//!` module docs stay short.
  - `//` explains why (a constraint, an ordering, a platform trap), at most two
    lines. Never narrate what the next lines do.
  - No citations of specs, reports, review items or section numbers (`§`).
- Keep work off the frame path: render only visible rows, reuse the layout
  cache, and never redraw for input that changes nothing (pointer motion). The
  loop wakes only for input, Agent events, an armed animation tick or a known
  expiry; an idle TUI must not tick.
- Bound everything that grows with a session: tool output tails, history
  windows, queues, undo steps, caches, held pre-install snapshots.
- Size limits come from what the protocol can legally carry, and are exported
  constants shared with their tests: `CONTROL_FRAME_MAX_BYTES` (64 MiB, admits a
  full 52,237,130-byte snapshot), `MAX_REQUEST_BYTES` (8 MiB, the Agent's request
  ceiling; admits a 1,573,049-byte maximum `input.push`) and `PRE_INSTALL_BUDGET_BYTES` (128 MiB,
  two frame ceilings). Never truncate or reject a legal input or frame to fit a
  bound, and never slice a body to a limit.
- Platforms: CI tests Linux, macOS and Windows. Put Windows-only behavior behind
  `cfg(windows)` at the call site, keep its pure logic compiled and tested on
  every platform, and never add latency or escape sequences to platforms that
  do not need them. Build paths with `Path`/`join`, use
  `std::env::consts::EXE_SUFFIX` for executables, and compare paths the way the
  platform writes them.
- Never put secrets in config, logs, errors, tests or docs; settings files name
  environment variables (`api_key_env`). Do not set environment variables on the
  Agent child: its tools would inherit them.

## Product Invariants

Keep these unless the user asks to change them:

- A loaded `Session` has one immutable Agent load and local epoch. Background
  reducers take that epoch; navigation alone changes `resident.current()`.
- Control state — execution, queue, pause, interaction, compaction, blocking —
  is written only by `App::apply_snapshot` from `session.state`, `session.open`,
  `session.create` and the `session_snapshot` event. Command receipts refresh it;
  they never write it. `turn.wait` reports terminal metadata and its direct
  successor, but never changes current execution. Unknown body identities are
  bounded until a snapshot confirms their region; canonical installation closes
  body appends while allowing auxiliary tool facts. History pins are never cached. Compaction inserts a marker and never
  removes transcript content. The footer shows only the context figure the Agent
  reported, never an estimate. Steering placement uses the delivered input ID and
  authoritative candidate request index, before its turn's terminal and successor;
  identical text never identifies an input or its boundary.
- The snapshot's completion directory is the discovery source: every entry above
  `settled_through` is read through `turn.result`, and only a fully verified body
  or an explicit partial/unavailable terminal region settles it. History catch-up
  settles a gap only after verified coverage of the entire newer pinned history,
  never merely because older pagination reached zero. A turn's canonical body is installed
  once; late deltas for it are dropped.
- An anchored view does not move when content grows; when the dock grows it
  gives up rows at its top so the row being read stays visible. Folding a block
  (click, `Ctrl+O`, `Ctrl+T`) pins the top of the view (`App::pin_view`), so
  blocks open and close downward. Folded thinking shows its newest rows.
- A selection takes only content. Rows and spans the UI adds around content
  (hints, spacers, timestamps, durations, turn-end lines, headers) are tagged
  with `rails::chrome` / `rails::chrome_span`; new UI rows must be tagged too.
- Local input acceptance has a bounded operation owner (64 operations / 16 MiB),
  separate from new drafts. Unknown acceptance is retained, never ordinary unsent
  history or automatically resent. F8 review keeps its original target and requires
  an explicit possible-duplicate confirmation before a new-ID resend; editing does
  not resolve the original uncertainty. Only confirmed acceptance enters history.
- Feedback that should not move the view uses the flash, not a dock notice.
- Config errors read `file:line: [table] key: reason` and exit with status 2
  before the screen changes. The TUI composes the Agent config; the Agent schema
  is not changed for the TUI's convenience.

## Tests

- A bug fix comes with a test that fails without the fix. Check it by reverting
  the fix once. Do not write tests that only restate the implementation or
  assert defaults, and name tests after the behavior they prove.
- Reducer behavior goes in `src/app/tests/<area>.rs` through `Harness`/
  `FakeAgent`; pure logic gets an inline `mod tests` next to it; rendering goes
  in `ui/snapshots.rs`. Do not add a parallel test file for one module.
- `FakeAgent::script(Value)` and `emit` deliver literal frames. Scenario helpers
  may explicitly publish a supplied snapshot and its query reply, but must never
  infer execution, queues or completions from command receipts.
- Tests are deterministic: use `#[tokio::test(start_paused = true)]`, scripted
  Agent responses and explicit gates, not sleeps. `testing::real_bounded` is an
  outer real-time failure watchdog only; correctness never depends on its timing.
- Do not compute `Instant::now() - duration` in tests: Windows counts `Instant`
  from boot and a fresh CI runner underflows. Assert the rendered text instead.
- Platform-neutral fixtures: no Windows reserved names (`nul`, `con`, `aux`),
  no hard-coded `/` paths in expectations, LF endings for byte-compared files
  (`.gitattributes`), and no non-UTF-8 file names outside Linux-only tests.
- Snapshot changes are reviewed as diffs of `tests/snapshots/*.txt`. Only
  `tests/built_in_agent.rs` starts the real Agent, and no test needs a provider
  or credential; real-model runs are manual.
- `e2e-observe` is a non-default acceptance build feature. Its opt-in trace
  observes actual canonical installation, global transcript order and saving
  markers. Its producer fails closed at row/event/file limits; default builds
  must contain no observer work or production setting for it. Product user-source
  identity metadata needed for request-boundary steering remains in default builds.

## Docs

A user-visible change updates the affected guide (`docs/keybindings.md`,
`docs/configuration.md`, the F1 help in `src/ui/views/help.rs`, `README.md`),
`docs/architecture.md` when a module or invariant changes, and the `Unreleased`
section of `CHANGELOG.md`. Describe what the user sees, not the review that led
to it. Do not rewrite released changelog sections.

## Backends, CI And Releases

- `Cargo.toml` pins `minicore-agent` by its release tag (`tag = "vX.Y.Z"`);
  `Cargo.lock` then fixes the Runtime the Agent pins. To adopt an Agent
  release, move the `tag` and `version`, run `cargo update`, and update the
  README backend line. Tags are never moved.
- CI runs Clippy and the tests on Linux, macOS and Windows with stable Rust and
  uploads a package per platform (`minicore-tui-<version>-<target>`) holding
  `bin/minicore-tui`,
  with `BUILD.txt` naming the three revisions.
- A release bumps `version` in `Cargo.toml` and the `minicore-tui` entry in
  `Cargo.lock` only, and moves `Unreleased` to the version in `CHANGELOG.md`.
  Tag `vX.Y.Z` (annotated) only after CI is green on that commit.

## Commits

- Subject: `type(scope): summary`, scope required, for example
  `fix(composer): keep the recalled draft when selecting`. Lowercase,
  imperative, no trailing period. Types: `feat`, `fix`, `refactor`, `perf`,
  `test`, `docs`, `build`, `ci`, `chore`. The scope is the module or area.
- One logical change per commit, with its tests and docs. Stage explicit
  paths; never `git add -A` in a tree with files you did not create.
- Do not rewrite pushed history, force-push, move tags or change the Git
  identity unless asked. Never commit secrets, local TUI directories
  (`session/`, `history.jsonl`, configs with real endpoints), or build output.
- Code, comments, docs and commit messages are in English.
