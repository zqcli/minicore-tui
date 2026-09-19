# v0.3 Refactor Delivery

## Delivery Boundary

The implementation is on `refactor/v0.3-full-project`, package version `0.3.0`.
The working tree was clean at the parent review. The branch has not been pushed.
Implementation and the remote Linux automated gates have been delivered;
**full cross-platform release acceptance is not complete**.

- Starting TUI HEAD: `9d11ee69c4efa02ef1e5bff143662b48dc3194de`.
- Final tested source revision: `9e399d9`.
- Documentation/evidence reviewed through: `c3c8c911a07b3f72a37b16086df5c287e3bdb224`.
- Fixed Agent: `061743369459299e66be97bf97d2b27352a39914` (0.5.0, protocol 1).
- Fixed Runtime: `6cd2bdbc634437dea925495c61c7eb0be10ba171` (0.4.1).
- All 131 commits through that evidence revision use repository-local identity
  `zqcli <zqcli@users.noreply.github.com>`. This delivery note is a subsequent
  documentation-only parent-review commit.

Agent/Runtime HEADs and tracked sources were rechecked unchanged. No Store
migration, backend upgrade, production Git write operation, approval mechanism,
plugin, PTY execution feature, or arbitrary shell entry point was introduced.

## Reviewable Commit Groups

The full commit list, including corrective commits, is available without a
network request:

```sh
git log --reverse --oneline 9d11ee69c4efa02ef1e5bff143662b48dc3194de..HEAD
```

| Stage | Representative commits | Delivered work |
|---|---|---|
| A | `444795c`, `d3be4cb`, `0725463`, `212785c` | Fixed sources, real-process fixtures, Rail/migration inventory, measured baseline and corrected evidence classifications |
| B | `e506061`, `61d8a8c`, `233c0fd`, `48156c6` | Protocol 1, pinned chunk reads, exact retained-result recovery, deferred preparation and compaction controls |
| C | `c843105`, `fe59a49`, `0450d64`, `ca622a4`, `58a6f3d`, `ed8bf81`, `4ab2767` | Nonblocking IO, catalog-only reload, shared sections, owned layout/decode workers, budgets, ToolFacts, source anchors and copy mappings |
| D | `86a9746`, `c6aeb87`, `179b299`, `80c9e1b`, `eefebcc`, `140469b` | Isolated drafts, browse/continue, command table, search/navigation/copy/export, UI preferences and external draft editor |
| E | `bd92e84`, `6653205`, `38d5367`, `e83a14b`, `a2071ed`, `5b597e1`, `733df37` | Tool streams, path references, files/grep, scoped read-only changes, explicit workspace status and Context |
| F | `c91da76`, `c118077`, `d5d1609`–`0aa64c5`, `daa944a`, `9e399d9` | Version/CI, lifecycle and resource review fixes, independent baseline, real OS-child pressure tests and corrected same-slave PTY validation |

## Retained And Replaced Responsibilities

Ratatui/Crossterm/tui-textarea and their framework versions remain. Rail
conversation geometry, editor/footer behavior, Unicode/IME mappings, native
selection, scrolling, TerminalGuard, background sessions, next-request model
updates and receipt-based Steer have retained or migrated regression coverage.

The old package-minor gate, production `session.history` loading path, staged
reload/history transaction, automatic cross-loop promotion of unsent Steer,
whole-durable-frame composition, synchronous clipboard/queue admission, and
long-lived raw-history/display-body duplication were replaced. Legacy history
DTOs remain only for explicit compatibility fixtures/diagnostics, not fallback.

Concrete ownership now lives in `app/{session,turn,history,queries,panels,
workspace,changes,context,search,export}.rs`, the corresponding state modules,
protocol adapters, and owned local jobs. No reusable UI/plugin framework was
introduced. Source sharing and release are verified with Arc identity/weak
references, capacity budgets, and production-path performance counters.

## RPC And Workflow Coverage

| Area | Consumed methods |
|---|---|
| Bootstrap/config | `agent.ping/reload/shutdown`, `model.list`, `profile.list` |
| Session lifecycle | `session.list/create/open/close/delete/rename/update/state` |
| Conversation authority | `session.read/presentation`, `turn.send/steer/cancel/wait/result` |
| Preparation/context | `session.context/compact/compact.cancel` |
| Tool details | `tool.read/output`, full ToolRef event facts |
| Workspace | `workspace.files/read/search/status` |
| Read-only changes | `changes.list/diff` |

User-visible breaking changes are documented in
[migration-0.2-to-0.3.md](migration-0.2-to-0.3.md): required Protocol 1
capabilities, explicit continue from read-only browsing, `/reload` versus
`/refresh`, quick `/new` with `/new form`, loop-scoped unsent Steer, and path-only
file references. Export overwrite and editor draft replacement are explicit,
owned, generation-fenced local operations; neither authorizes model shell IO.

## Independently Checked Evidence

The parent checked the remote source manifest and final logs, reviewed key
protocol/state/IO/panel paths, and required corrections to fixture provenance,
acceptance claims, result recovery, clipboard ownership, layout integration,
export races, CI offline fetching, and the PTY measurement itself.

- Rust 1.85.0 and stable remote Linux: **830 passed / 0 failed / 53 ignored**
  each; fmt, strict Clippy and warning-denied rustdoc passed.
- Fixed-Agent serial loopback E2E: **34/34 on each toolchain**.
- Release performance: **9/9 on each toolchain**; the separately invoked
  ignored tests are not silently included in the default-test pass count.
- 51,101 stable display rows / 1000 actual deltas: zero stable-history layout
  calls and zero historical-body cloning.
- Rust 1.85 near-256 KiB App editing: P95 **700 µs**, P99 **755 µs**;
  identical direct-Composer baseline/current P95 **1492/192 µs**.
- 120×40 synthetic frame workload: P95 **3571 µs**, P99 **4367 µs**.
  These are not terminal input-to-frame latency measurements.
- A real two-second clipboard child and a real OS child producing stdout
  while not reading stdin passed nonblocking, bounded-admission, FIFO and
  cleanup checks.
- Linux kernel-PTY restore/panic/editor/input/resize/shutdown tests passed;
  the negative raw-mode fixture failed the same-slave cooked check as expected.
  Thirty idle seconds produced only two initial draws.
- Current 375-entry source/scripts/tests/snapshots manifest matched remotely:
  `607a4b52d4b865b6210f8865473f3a8aa8126b15d374b604f050fc4ebb09ba00`.

[Final F evidence](verification/v03-f/README.md) records log locations and
measurement boundaries. [The acceptance matrix](refactor-acceptance.md) records
55 Passed and REF-55 Not run. Production-source/test/script/manifest/snapshot
`git diff --check` passed; unmodified raw verification logs retain their emitted
EOF blank lines, and the supplied Spec retains its Markdown hard-break spaces.

## Remaining Release Gates And Execution Deviation

Hosted CI and native macOS/Windows execution have not run. The six-way
OS/toolchain matrix and pinned-backend integration job are configured; the
latter was reproduced with a fresh remote Cargo home, but this is not a hosted
run. Manual iTerm2/IME/clipboard acceptance, external Provider smoke testing,
terminal input-to-frame latency, exact allocator accounting, and generation of
a real fixed-Agent history item above 8 MiB remain unclaimed. Oversized-item
behavior has deterministic bounded-fixture coverage and an explicit raw export.

Phase F once violated the original remote-only requirement by compiling and
running Rust locally. That deviation was disclosed; the local results are
excluded from delivery evidence, and the affected validation was rerun on the
authorized remote builder. Existing local artifacts were not deleted to hide
it. Final accepted Rust/Cargo evidence is remote-only.
