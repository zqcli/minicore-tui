# Historical v0.3 E3 — Changes/Diff, explicit workspace status and Context

This directory is a frozen E3 evidence record for the pre-final-F source tree.
E3 implementation and its automated checks were complete at that boundary.
**Parent E3 review remains pending; the record is not final-source release
acceptance.** Current local evidence and remaining statuses are tracked in
[`../../refactor-acceptance.md`](../../refactor-acceptance.md).

## Source and authorization boundary

- Branch: `refactor/v0.3-full-project`; accepted E2 start: `95d3a27`.
- `5a7c62b`: fixed changes/diff/status DTOs and bounded fragment/layout foundation.
- `a2071ed`: concrete Changes view, shared queries/layout and explicit status Footer.
- `5b597e1`: Context surface over B's operation owner, bounded diff hardening,
  nine snapshots, deterministic tests and two new real-Agent scenarios.
- `001a603`: retired-context confirmation scheduling, no zero-deadline spin,
  panel-only observation retirement and realistic harness timer delivery.
- Final tested implementation: **`733df37276a6475fb0af2f4f7ee362bea7fd0ba5`**:
  `/refresh` targets Changes/Context rather than reloading conversation history.
- Agent: `061743369459299e66be97bf97d2b27352a39914`.
- Runtime: `6cd2bdbc634437dea925495c61c7eb0be10ba171`.

Contracts were checked against the fixed Agent's `docs/rpc.md`,
`src/{changes,diff,sessions,compaction}.rs`, `src/workspace/status.rs` and
`src/compaction/recovery.rs`, not inferred from method names. Backend sources,
Store formats and executable are unchanged. Existing untracked backend spec
files were preserved. No model/reasoning/context preference was changed;
the user's `gpt-6-astra:high` / default-context preferences remain untouched.
No local Cargo build/test/format, push, backend upgrade or production Git write
was performed. Git writes in the new E2E module are confined to disposable
synthetic repositories.

## Implemented boundaries

### Changes and Diff

- `/diff` defaults to workspace; `/diff session` and `/diff turn <loop_id>`
  request explicit native-change scopes. Workspace origin remains unknown.
  Session/Turn descriptions cover native write/edit/apply_patch evidence only,
  never inferred attribution for Bash, a user, or an external editor.
- One concrete `MainView::Changes` retains its list and at most one selected
  comparison. Esc goes diff → list → conversation with both positions, the
  draft and conversation anchor retained. No router or navigation stack.
- `changes.list` sends limit 100 / max_bytes 65536. Records are identified by
  their original opaque ref, never merged by path. ToolRefs stay independent.
  Kind, origin, commit/coverage/detail flags, consistency, source completeness,
  local limits and errors are shown without inventing completeness.
- Workspace comparison defaults to `head_to_worktree`; Tab cycles all three
  workspace comparisons. Native records stay `tool_before_after`. Comparison
  changes clear body/cursor and advance generation. Diff requests use the
  original ref, context_lines 3, max_bytes 65536 and exact returned cursor JSON.
- Fragments validate hunk/line identity, raw byte offset, length, UTF-8 and
  completion. Only `line_complete` completes a logical line. Gaps, duplicated
  or reordered lines/hunks, version mismatches and overflow cannot silently
  concatenate. Changed/stale retains old content and requires explicit refresh.
  Binary/unavailable/truncated states are explicit. Version displays are real
  typed revisions (SHA abbreviations are visibly abbreviated), not provider
  revision tokens. Workspace diff-time versions may refresh list-time metadata.
- Raw CRLF and no-final-newline are preserved. Copy means **line-source text
  from the displayed immutable layout snapshot**, concatenated in Agent order,
  without hunk/+/−/line-number/color/soft-wrap decoration; it is **not an
  applicable patch export**. Partial lines, stale data, pending newer layout
  and display limits are disclosed. Copy waits for a current-width layout.
- Central bounds: 500 records / 1 MiB record accounting; 1 MiB diff body,
  16,384 logical lines, 131,072 layout rows. Immutable coalesced source buffers
  feed the existing serialized layout worker. Retained text/index capacity
  participates in the existing 48 MiB layout budget; pathological graphemes
  and display limits remain explicit.
- Changes and status share the existing two read slots and deferred budget.
  Old requests retain their real slot until completion; generations/epochs
  fence late data and typed Tool/File/Diff layout results. Hidden comparisons
  stop reads. Renderers perform no I/O.

### Footer observation

`workspace.status` is explicitly requested on execution-session open (event/ACK
coalesced by epoch), Changes open and explicit Changes refresh only. There is
no per-token/tool/draw status query or TUI Git subprocess. The full DTO is
decoded; the Footer retains metadata, not a duplicate entries/path list.
Metadata retention across sessions is capped at 1 MiB.

The Footer uses this observation instead of `session.presentation.git_branch`.
A confirmed non-repository keeps the plain workspace identity; unknown,
incomplete, `seen:branch`, detached and `stale:branch` are distinct. The
seen/stale prefix survives narrow clipping. Failure never becomes NoGit and
keeps a prior successful observation marked stale. These are last-observation
facts, not a filesystem watcher or a claim of current repository cleanliness.

### Context and B operation ownership

- `/context` opens a concrete main view with separate coverage, estimated
  history/request budgets, automatic current/last preparation, manual operation
  and utility usage sections. Missing values remain unknown. No summary body
  is rendered or inserted into Prompt/History. Nine snapshots cover all three
  new surfaces at 60x16, 80x24 and 120x40.
- F6, local Tab/Enter actions, Page/arrows/End, wheel and scrollbar remain local.
  Closing Context never sends cancellation. Compact admission reuses the
  loaded/idle/settled/unblocked B checks, process-unique operation counter,
  exact operation cancellation and unknown-write state/context confirmation.
  Failed results retain history. Compacted/noop refresh only context, not pins
  or history. `/refresh` also targets the current new main view.
- Panel reads, B execution polling and required settlement reads share the
  existing context/query owner. Panel-only polling stops on close or no active
  operation. An unmaintained active panel snapshot is discarded, not left as
  a permanent execution fence; authoritative state and real B operations stay.
  B-owned in-flight work/confirmations survive panel close. Polls are no faster
  than 500 ms foreground / 2 s background; switching sessions moves the old
  foreground deadline to the background interval. Idle reads are explicit.
- Older in-flight reads block a second context read but retain their slot.
  Their completion resumes due confirmation, without a zero-deadline timer
  loop. Panel refresh cannot replace B's settlement owner. `method_not_found`
  disables the corresponding context/compact/cancel entry and never falls
  back to turn cancellation or a shell command.

Optional Tool→Changes and Diff→FilePreview links were not added; no unsupported
link, hidden navigation graph, automatic attachment, Git mutation or new worker
is advertised.

## Final remote validation

All commands ran on `root@192.168.20.199` under
`/root/minicore-tui-v03-refactor/tui`. The credential-free `e3-verify.sh` is the
actual script, and `e3-verification.log` records all completion markers. It
checks disk space before builds; its only possible cleanup is the exact TUI
incremental cache. The final run had about 40 GiB free and needed no cleanup.

| Check | Rust 1.85.0 | stable 1.97.1 |
|---|---|---|
| fmt --all -- --check | Passed | Passed |
| test --locked --all-targets --no-fail-fast | **812 passed, 0 failed, 43 ignored** | **812 passed, 0 failed, 43 ignored** |
| Clippy --all-targets -- -D warnings | Passed | Passed |
| rustdoc --no-deps, RUSTDOCFLAGS=-D warnings | Passed | Passed |
| Serial fixed-Agent E2Es, including all previous 32 | **34/34** | **34/34** |

All **six** ignored Release performance workloads passed on Rust 1.85.0.
Of the 43 default ignores, 34 Agent E2Es and six performance workloads ran
separately. The manual scrollbar benchmark and two real-TTY lifecycle tests
remain **Not run**. Empty fmt logs indicate successful checks. Dependency
inspection is recorded; E3 added no dependency.

New coverage is in `tests/changes.rs` (8), `src/app/changes_tests.rs` (6),
`src/app/context_tests.rs` (9), one nine-snapshot test and two Agent E2Es:

- NoGit → unborn → staged/unstaged/untracked/rename/delete/conflict/binary →
  detached observations; multiple list pages, stale list cursor, all three
  comparisons, a 140 KB single-line UTF-8 comparison over raw fragments,
  CRLF/no-eol, mutation during paging, binary and the shared layout worker.
  This workspace scenario asserts **zero Provider calls and zero History**.
- Two native writes to one path retain independent refs/ToolRefs and exact
  Turn scope; Bash and external changes do not become native records. Historical
  tool comparison uses `tool_before_after`. Opening Context reads real metadata
  without new Provider calls, History items or lost draft text.

## Failures retained and corrected

`e3-cancel-full-red.log` preserves the initial 33/34 result: the existing manual
compact-cancel E2E waited for a fresh context confirmation behind a retired
read, while the old harness did not deliver the application's quiet-period
context deadlines. `e3-retired-context-red.log` independently reproduces the
zero-deadline spin with deterministic reducer input. The fix preserves actual
slot ownership, resumes due work on completion, and makes the harness follow
real `next_tick()` deadlines. No assertion, Provider deadline or test timeout
was relaxed. Both complete final toolchain runs passed all 34 scenarios.

`e3-context-poll-red.log` is an earlier scheduler checkpoint that exposed a
foreground deadline surviving a background transition; final tests also
separate panel-only reads from B-owned execution/confirmation polling.
`e3-foundation*.log` and `e3-refresh-check.log` are intermediate scoped checks,
not final-suite counts. The native E2E originally assumed positional record
order; it now selects the exact tool-call identity, without weakening content
checks. Five pre-existing snapshots changed only their unknown-git Footer text;
nine E3 snapshots are new. The final verification was rerun after the last
`/refresh` routing correction.

## Performance and remaining validation

Final `e3-performance.log`:

```text
c2b_worker: durable_rows=51101 deltas=1000 layout_calls=0
            history_bytes_cloned=0 viewport_rows=40000 viewport_bytes=2986911
c2c_120x40: p95_us=7999 p99_us=8335 durable_rows=43870
            history_bytes_cloned=0 layout_calls=0 viewport_rows=40000
            viewport_bytes=4396336 retained_layout_bytes_estimate=8035080
```

The structural constraints remain satisfied. These are fixed synthetic frame
samples, not terminal input latency or proof of optimization over E2's
7500/7951 μs. No fresh peak-RSS or end-to-end diff latency was measured.
Manual iTerm2/IME/clipboard/real-TTY, macOS/Windows and a real external Provider
remain **Not run**. General oversized (>8 MiB) automatic history reading is
unchanged. Parent review and F overall validation remain separate.

## Exact source equality

`e3-source.sha256` covers all **368 tracked build/test/snapshot inputs** under
`.cargo`, Cargo manifests/lock, source, tests, snapshots and vendor. Both
`e3-source-check.log` (remote tested bytes) and `e3-local-source-check.log`
(local final bytes) contain 368 successful checks. Manifest SHA-256:

```text
fa7ea80e11cf9aa642a5a7ea7352bb9d3620bf6b7863e6e29ec1294319be0f2d
```

`e3-agent-source.sha256`, `e3-runtime-source.sha256`, their check receipts and
`e3-environment.log` verify fixed backend inputs and the unchanged executable:

```text
661b32976ad6ae2fbe2b33411c7d0d082f9782da70745e4e4a6602c87fb7b273
```

The executable's original build provenance remains in
`../v03-e1/e1-fixedagent-build.log`. The following evidence/documentation-only
commit does not change these tested inputs. No push was performed.
