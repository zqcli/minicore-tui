# v0.3 E2 — workspace references, preview and literal search

E2 implementation and automated verification are complete; parent review is
pending. E3 Changes/Context main-area work has **not** started. This is not a
full-project v0.3 release or manual/three-platform acceptance.

## Source and scope

- Branch: `refactor/v0.3-full-project`.
- Start: `285dbd10b3f622d6820e155753cb8637a60b0e92` (accepted E1 baseline).
- `38d5367`: pinned workspace DTOs, bounded source-mapped file layout, schema tests.
- `e83a14bb9eef267337e9d8e490fc456cb82c2054`: concrete Dock/FilePreview,
  references, literal search, shared query/layout integration, E2Es and snapshots.
- Final tested implementation: `d553e96d9ae32b540e9e3c4c0e03af3974e89634`:
  explicit Dock/Search focus and hardware input-caret positioning in cells.
- Agent: `061743369459299e66be97bf97d2b27352a39914`.
- Runtime: `6cd2bdbc634437dea925495c61c7eb0be10ba171`.

Read contracts: the fixed Agent's `docs/rpc.md` workspace.read/files/search
sections and `src/workspace/{query,listing,search,scan}.rs`, including request
validation, cursor fields, scan-stop/entry-kind enums and loaded-session rules.
No backend source, Store schema, Provider configuration, model selection or
context defaults were changed. Existing untracked backend specification files
were left intact. No local Cargo build/test/format operation and no push ran.

## Implementation boundaries

- `src/protocol/workspace.rs`: concrete read/files/search DTOs and requests.
  Unknown additive result fields are tolerated; malformed known fields fail.
  Listing/search cursors remain opaque JSON, including additive fields.
- `src/app/workspace.rs`, `src/state/workspace.rs`, `src/ui/workspace.rs`:
  one concrete temporary workspace Dock and one bounded FilePreview, not an IDE,
  generic panel framework, router, filesystem scanner or second RPC owner.
- An exhaustive `close_main_detail` in `src/app/panels.rs` closes only the
  selected main variant, restores its conversation anchor and invalidates its
  read scope. Tool-only close cannot take/drop FilePreview. A file can retain
  one results-Dock return target; there is no navigation graph.
- All workspace entry points require a loaded Session. Closed browsing explains
  explicit Continue, never issues session.open automatically, and never derives
  a local filesystem root from a returned path.
- Word-boundary typed `@` and `/files` offer paths; `/grep` offers literal text
  search, an optional single path/JSON array (at most 32), and case sensitivity.
  Query/scope edits debounce 150 ms. One browser request and one file request
  can be outstanding; both share the existing two slots and deferred budget.
  A closed/stale query retains its real slot until its actual completion.
- Browser generation changes clear the cursor. Only received candidates are
  sorted, with selected-path identity preserved. Cursorless/deadline responses
  do not rescan automatically. Last-page partial, scan-complete, stop and skipped
  metadata are shown, alongside the local 500-item/1 MiB retention limit.
- Reference insertion uses readable reversible JSON quoting, including unsafe
  controls, and one native editor insertion (no u16 cursor jump). Only path text
  becomes draft text. The optional last-insertion range supports Editor F4;
  any content edit invalidates that mapping and leaves ordinary text, not an
  attachment. Preview never injects content into Prompt, summary or History.
- File reads start at line 1, byte offset 0, 400 lines, 65536 encoded bytes.
  Continuations use the exact returned range and original revision, including
  same-line offsets. Only `ok` appends. `changed` keeps old data and stops;
  binary/too-large/unavailable are explicit, not empty successful previews.
- Raw file text retains CRLF and no-final-newline, with a 512 KiB body bound and
  small coalesced immutable chunks. Layout/safety/wrapping/source indexing use
  the **existing serialized layout worker** and 48 MiB layout budget. Render
  materializes visible indexed rows only. Pathological oversized graphemes have
  an explicit bounded display placeholder; safe-source copy remains available.
- File copy has no line-number/soft-wrap decoration, preserves normal source
  line endings, and visibly escapes unsafe controls. Pending/newer source bytes
  never silently change the copied immutable snapshot; partial/old data is
  disclosed. Grep byte ranges are checked at UTF-8 boundaries and highlight
  complete terminal graphemes, including combining marks.
- Dock/selection/main focus precedes root cancellation. F6, scrolling, follow,
  mouse scrollbar, copy, refresh and Esc stay local. Query caret position uses
  safe-text cell widths for IME. Query/path/content owners have redacted Debug.

## Remote environment and commands

All builds and checks ran on `root@192.168.20.199` in
`/root/minicore-tui-v03-refactor/tui`:

- Linux 6.12.94+deb13-amd64 x86_64; 24 logical CPUs, 31940 MiB reported RAM.
- Rust 1.85.0 and installed stable 1.97.1.
- Fixed executable: `/root/minicore-tui-v03-refactor/fixedagent-target/debug/minicore-agent`.
- Binary SHA-256 (unchanged from E1):
  `661b32976ad6ae2fbe2b33411c7d0d082f9782da70745e4e4a6602c87fb7b273`.

`e2-environment.log` records binary/environment facts and successful tracked
backend source/Cargo-input comparisons against the fixed local checkouts;
`e2-agent-source.sha256` and `e2-runtime-source.sha256` preserve those inputs. E1's
fixed-Agent build provenance remains in `../v03-e1/e1-fixedagent-build.log`.

The credential-free `e2-verify.sh` is the actual verification script. It records
disk space before builds and deletes only the disposable exact TUI directory
`/root/minicore-tui-v03-refactor/tui/target/debug/incremental`, never another
project's targets or backend/Store data. The final run had 41–44 GiB free.
`e2-verification.log` contains the final script's completion markers.

| Check | Rust 1.85.0 | stable 1.97.1 |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Passed |
| `cargo test --locked --all-targets --no-fail-fast` | **788 passed, 0 failed, 41 ignored** | **788 passed, 0 failed, 41 ignored** |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed | Passed |
| `RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps` | Passed | Passed |
| Fixed-Agent E2Es, ignored tests run serially | **32 passed, 0 failed** | **32 passed, 0 failed** |

Each toolchain has its own `e2-<toolchain>-{fmt,tests,clippy,doc,e2e}.log`.
An empty fmt log means the checked command succeeded, not that it was omitted.

All six ignored Release performance workloads also passed on Rust 1.85.0.
Of the 41 default ignores, 32 Agent E2Es and six performance workloads were
executed separately; the manual scrollbar benchmark and two real-TTY lifecycle
checks remain **Not run**. Dependency inspection is `e2-dependencies.log`; E2
adds no dependency.

## New test coverage

- `tests/workspace.rs`: **8** tests for actual pinned fixtures, additive and
  malformed fields, deadline/no-cursor, raw CRLF/UTF-8/source ranges, tiny chunks,
  large lines, unsafe controls, path-token roundtrips, grapheme-safe match
  highlighting and bounded pathological graphemes.
- `src/app/workspace_tests.rs`: **17** deterministic reducer/render tests:
  debounce/generation/opaque cursors, retained real slots, Tool/File switching,
  changed/binary/too-large/unavailable responses, exact same-line continuation,
  closed-session gating, reference-only input, native undo, 70,000-character
  paste/cursor retention, scope/case changes, selected-match positioning,
  500-item retention, malformed ranges, focus/IME cells, local scrollbar drag,
  live-loop no-cancel, draft/anchor restoration and 60x16/80x24/120x40 rendering.
- `ui::snapshots::workspace_e2_panels`: **1** test, four **new** snapshots:
  narrow dark file, light file, files Dock and grep Dock. No existing snapshot
  was replaced or reformatted; Rail/Editor/Footer geometry remains unchanged.
- `tests/agent_e2e/workspace.rs`: **2** new real-Agent E2Es, both run on each
  toolchain in addition to all previous 30:
  - 230 Unicode/space/quote paths across three files pages; literal grep across
    three pages with 231 matching records, binary/oversize skips, explicit scope
    and case, safe path insertion and selected source-line 1200 positioning;
  - a 140 KB UTF-8 single line across exact same-line continuations, CRLF and no
    final newline; mutation between pages returns changed without concatenation;
    explicit refresh, binary and too-large states. The real shared layout
    worker is used. Both E2Es assert **zero Provider calls and zero History items**.

Intermediate evidence is labelled by scope: `e2-foundation-tests.log` and
`e2-foundation-clippy.log` validate the first slice (768 default passes);
`e2-new-e2e.log` and `e2-new-snapshots.log` are targeted checkpoints, not the final
suite counts. Final counts always come from the two complete toolchain logs.

## Corrections found during verification

- `e2-foundation.log` is the retained red test: the old source-derived
  `workspace-files-deadline.json` fixture inherited a paging cursor. Fixed Agent
  `listing.rs::an_expired_budget_returns_no_entries_and_no_cursor` and rpc.md
  require no cursor. The fixture was corrected, not the backend or assertion.
- The first candidate E2E wrongly assumed local sorting selected index zero
  after later pages. The implementation preserves the user's highlighted path,
  so the test now selects/validates that actual highlighted identity. It passed
  again against the real Agent; no product assertion or timeout was weakened.
- Normal exhaustive-match compile fixes and one Clippy nested-if cleanup were
  completed before final verification. The final logs have no failed tests or
  warnings; early diagnostics are not described as successful checkpoints.
- Final UI inspection added explicit Dock/Search focus and cell-based input
  caret positioning. The entire verification script was rerun after this fix.
  This is automated caret geometry evidence, **not** manual IME acceptance.

## Performance evidence

From final `e2-performance.log`:

```text
c2b_worker: durable_rows=51101 deltas=1000 layout_calls=0
            history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=2986911
c2c_120x40: p95_us=7500 p99_us=7951 durable_rows=43870
            history_bytes_cloned=0 layout_calls=0 viewport_rows=40000
            viewport_bytes=4396336 retained_layout_bytes_estimate=8035080
```

These are the existing fixed synthetic structural/frame workloads, not terminal
input latency. E1 was 7941/8306 μs; scheduling differences are not claimed as an
optimization. The synchronous cold-prepare/all-lines clone diagnostics remain
in the full log and are not the production serialized-worker hot path.
No fresh peak-RSS measurement or end-to-end file-preview latency was made.

## Exact source equality

`e2-source.sha256` covers all **349 tracked build/test/snapshot files** under
`src`, `tests`, `snapshots`, `.cargo`, plus Cargo.toml/Cargo.lock. It includes the
new test module and all four new snapshots. Documentation/logs are excluded to
avoid a self-referential manifest. SHA-256 of the manifest:

```text
57b1833eb215aa1266989fec3b8a297cfdca94cba365ed3c33cc48ee8c1ebc18
```

The local final source and the remote tested tree both passed a complete
`sha256sum -c --quiet` check; `e2-source-check.log` records the remote receipt.

## Not run / remaining

- Parent E2 review: pending; no E3 Changes/Context main pages were implemented.
- Manual iTerm2/IME/clipboard/mouse/real-TTY restore acceptance: **Not run**.
- macOS/Windows compilation and execution; full three-platform acceptance:
  **Not run**. Only the prescribed remote Linux host was used.
- Real external Provider testing, real terminal input-to-frame latency and new
  peak-RSS/allocation measurement: **Not run**.
- The existing oversized-history (>8 MiB) general read-workflow limitation is
  unchanged. No Store migration, approval, regex/shell search, local workspace
  scanner, implicit attachment, watcher/index, or backend upgrade is hidden here.
