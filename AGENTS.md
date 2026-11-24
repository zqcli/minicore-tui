# AGENTS.md

Guidance for coding agents in this repository. Humans should start with
[README.md](README.md). Source and [docs/architecture.md](docs/architecture.md)
define behavior; when this document disagrees with code, inspect the behavior
and fix the inaccurate description.

## Project and boundaries

`minicore-tui` is a Rust 2024 fullscreen terminal frontend using ratatui and
crossterm. It embeds MiniCore Host 0.12.11 and calls its typed Rust client through
`HostClient::in_process`. The executable contains the native resource builder,
provider implementations and built-in tools. Host owns execution, credentials,
Agents, Sessions, persistence, compaction and workspace operations. The TUI owns
terminal interaction and presentation.

There is no child Agent process, stdio RPC adapter, frontend protocol DTO layer,
Webpage server, browser assets, phone protocol or remote-control mode. Do not
restore them as compatibility shims. A backend defect belongs in Host, not in
a frontend imitation of Host state. Only change a backend repository after
confirming that is where the behavior belongs.

See [docs/design-host.md](docs/design-host.md) for this migration's contracts.
Historical design notes are not the current interface specification.

## Repository map

| Path | Responsibility |
|---|---|
| `src/main.rs` | Startup, terminal loop, frame pacing, signals and Host shutdown |
| `src/host.rs`, `src/host/` | Persistent embedded Host, production resources, diagnostics |
| `src/config/`, `src/cli.rs` | Local JSON preferences, shared configuration paths, explicit startup selection |
| `src/app/` | Reducer, actions, original-target async work, input, overlays and navigation |
| `src/app/control.rs`, `src/app/session/` | Native state/observation projection and canonical installation |
| `src/app/tasks.rs` | Typed Host operations and bounded event delivery |
| `src/app/unconfirmed.rs` | Local acceptance uncertainty and explicit resend review |
| `src/history.rs`, `src/transcript/` | Native paged history/export, positioned blocks, tools and layout cache |
| `src/ui/` | Rendering, sheets, views, status, Markdown and themes |
| `src/terminal/` | Platform input decoding and frame batching |
| `src/testing.rs`, `src/app/tests/` | Real in-process Host reducer tests with explicit resource/model doubles |
| `src/ui/snapshots.rs`, `tests/snapshots/` | TestBackend frame assertions |
| `src/ui/perf_tests.rs` | Optional frame-cost checks |
| `tests/built_in_host.rs` | Production provider/resource/persistence/shutdown integration |
| `docs/` | Current architecture, configuration, keys, UI spec and migration design |

UI rendering performs no Host calls or filesystem I/O. `ui::draw` updates only
presentation caches, selection resets and editor hit geometry.

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
cargo clippy --locked --all-targets --target x86_64-pc-windows-gnu -- -D warnings
UPDATE_SNAPSHOTS=1 cargo test --locked --lib ui::snapshots
cargo test --locked --lib perf_tests -- --ignored --nocapture
```

Use `--locked`. Dependency resolution changes are intentional changes, followed
by full checks. The pinned Host lock baseline may include yanked versions that
are valid locked dependencies; do not silently change the backend graph to
work around a fresh resolver result. `MINICORE_TUI_PERF_STATS=1` reports live
frame counts/timings on exit.

## Code rules

- Use current stable Rust; `unsafe` is forbidden. Dependencies use plain version
  requirements, with immutable Git revision pins for native Host crates.
- Write the least code that solves the problem. No speculative backend trait,
  generic operation framework, compatibility adapters or duplicate lifecycle.
- Validate at the boundary that owns input. Respect native typed identities and
  invariants; do not hide actual faults with invented defaults or fallbacks.
- Delete obsolete code, tests for removed wire contracts and unused parameters.
  Keep useful terminal and execution regression coverage when replacing a backend.
- Keep module docs short. Comments explain constraints or platform traps; they
  do not narrate code, cite review checklists or embed specification sections.
- Preserve incremental rendering, Markdown, editor/paste behavior, anchors,
  selection and caches. Do no work on pointer motion that changes nothing.
- Idle views have no periodic timer. Bound transient input ownership, event
  queues and caches; release completed output buffers. Do not claim constant
  memory while retaining an arbitrary loaded transcript.
- Limits belong to the actual resource being bounded. Do not carry old JSON-RPC
  frame ceilings into the in-process API or truncate legal native content.
- Apply presentation limits before allocating discarded output. Measure native
  event/history installation as well as draw-only renderer fixtures.
- Windows-only behavior stays behind `cfg(windows)` at its call site; compile
  and test pure logic everywhere. Use `Path`/`join`, executable suffix constants
  and native path formatting. Do not add platform-specific latency elsewhere.
- Never put credentials into config examples, logs, test fixtures or errors.
  `api_key_env` names the source; production resource preparation reads it.

## Behavior contracts

- Session addresses include Agent ID and Session ID. Host incarnation, local
  epoch, Load, Turn, response and tool identities keep async work on its original
  target. Terminal navigation does not change execution ownership.
- Merge controller and runner revisions independently. A runner belongs to its
  Load. Late Turn results cannot replace a later active Turn or interaction.
- Native snapshots own execution, queues, pause, residency, interaction,
  compaction, persistence and blocking. Mutation receipts cause reads, not
  synthetic success. `Persisted` alone means saved.
- Catalog reads use one coalescing owner. Automatic workspace status reads are
  separate from unrelated controls and skip Sessions with no workspace.
- A send is never automatically repeated. Pending/unknown acceptance retains the
  original target and text. F8 resend needs explicit duplicate confirmation and
  a fresh Message ID. Only verified acceptance enters prompt recall.
- Session creation owns its first input independently of current terminal focus.
  Delayed creation/opening results cannot send a draft into another Session.
- Use native OutputBuffer range/UTF-8 recovery. Replace streaming content with
  canonical positioned records; suppress late replay without retaining duplicate
  full bodies. Tool arguments are complete native facts, not generated previews.
- Use fixed native history windows and opaque cursors. Do not invent cursors,
  infer recovery from unrelated pagination, collapse repeated text/Message IDs,
  or silently discard canonical content. Full export has its own paged reader.
- Pause/resume, load/unload and close remain distinct. Esc cancels the original
  Turn, preserves queued input and does not synthesize a pause. Approval allows
  once or denies; persistent policy changes use native Session configuration.
- Agent deletion confirms the exact child scope. Do not retry with an expanded
  scope when the catalog changes. Admission is not completed deletion.
- Context and status display native facts. Last-response usage is not a context
  percentage. Show actual retry, partial result, persistence and shutdown state.
- Preserve scroll anchors when installing history or folding. Selection copies
  content only; mark rails, hints, timestamps and decorations as chrome.
- Host owns directory parsing and declared Agent identity. TUI reads only its
  own `tui.json`; reload carries no candidate. Declared defaults do not rewrite
  existing Sessions. Dynamic Agent copies omit source metadata and Session
  overrides. Files are not watched implicitly.
- Keep the Host and data-root lease until actual shutdown completion. Timeout,
  lost observation or a save fault does not prove success. Force shutdown needs
  the user's explicit shutdown-prompt action.

## Tests and documentation

Use real Host and native SDK objects for frontend execution tests. Only resource
or model boundaries may use explicitly named controllable doubles. Use gates
and actual events for synchronization, not correctness sleeps. `real_bounded`
is an outer real-time failure watchdog. Renderer fixtures prove rendering only.

Keep CJK/multiline paste, keyboard, selection, scroll and performance regression
coverage. Avoid subtracting durations from `Instant::now()` in tests; fresh
Windows runners can underflow. Fixtures use portable paths/names and LF endings.
Snapshot updates are reviewed as diffs. No test requires live provider access.

A visible change follows [docs/ui-spec.md](docs/ui-spec.md) and updates the
relevant guide, F1 help, README and changelog;
module/contract changes also update architecture. Preserve released changelog
entries. Remove obsolete active documentation instead of presenting it as a
supported configuration path.

## CI, release and commits

CI checks Linux, macOS and Windows and packages Linux x86_64, macOS arm64,
macOS x86_64 and Windows x86_64. `BUILD.txt` records the TUI revision and pinned
Host revision. Tags are never moved. Annotated release tags are created only
when requested, after checks pass on that commit.

Commit subjects use `type(scope): summary`, lowercase, imperative, no trailing
period. Types are `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`
and `chore`. Commit one logical change with its relevant tests/docs. Stage
explicit paths; do not rewrite pushed history, force-push, change identity or
include unrelated work. Never commit build output, private runtime files or
credentials. Code, documentation, comments and commit messages are in English.
