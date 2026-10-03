# TUI 0.3.0 closeout verification

Date: 2026-10-03. Status: implementation, golden review, final integrated remote
quality gates, staged-source review and dev fast-forward passed. Both branches
were published with a non-force atomic push; hosted CI passed on Linux, macOS
and Windows with Rust 1.85.0 and stable, plus quality and fixed-backend E2E jobs.
The earlier default-identity permission blocker was resolved without changing
credentials or repository authentication configuration.

## Scope and fixed inputs

- TUI remains version 0.3.0. Starting HEAD:
  `645369093f9020a427685134dfc9783166f15772` on
  `refactor/v0.3-full-project`.
- The 30 pre-existing tracked layout changes (+634/-212) are explicitly adopted
  closeout scope, not excluded legacy work. The starting index was empty.
- Prior dirty-tree evidence: 55 failures (49 UI snapshots and six
  editor/help/reference/render cases), plus 13 formatting-drift paths and four
  Rust 1.85 strict-Clippy diagnostics. Each must be resolved, not waived as old.
- Agent 0.6.0: `d81728b13db68c76b05b4c8cb87161770769947f`.
- Runtime 0.6.0: `9e230617d36130e7ec77aba122b45f1347ac53f2`.
- No backend source changes, new TUI version/tag, binary deployment or paid
  provider requests are authorized by this closeout.

## Verification rules

All Rust/Cargo, reference-fixture generation and snapshot generation run remotely.
QA owns the authorized Linux builder. Every export records its exact file-content
manifest and uses source-specific targets; neither reused target mtimes nor test
counts alone establish provenance. Final evidence must match the integrated,
reviewed source and staged changes.

Snapshot updates require a settled fixed left-gutter/body/right-gap/scrollbar
contract and independent before/after review. Scrollbar visibility must not
reflow the body. Behavioral checks retain Unicode, cursor, selection, resize,
fold/resume, hit testing and retained-frame coverage.

Required gates: stable and Rust 1.85 full tests, fmt, strict Clippy and
warning-denied rustdoc; the fixed real-Agent 35-case serial mock E2E suite;
applicable existing ignored performance and Linux kernel-PTY checks. Existing
hosted Linux/macOS/Windows jobs are observed after safe non-force delivery.

## Reproduced baseline (not final acceptance)

The remote immutable export `src-6453690-dirty-ba58d2c5` reproduced the same
failure set on stable (Rust 1.97.1) and Rust 1.85.0: **1130 passed, 55 failed,
54 ignored**, both commands exiting 101. Its source-manifest SHA-256 is
`dabb624ad2172450ae8edcef23969a4385d48323716df872159ba1083c73f915`.
The worktree-patch SHA-256 is
`ba58d2c5477922ea9bb0a181bc3fb36f8cb70406a93e194361c2a6277b1ef70f`.

The 49 `ui::snapshots` failures include 48 before/after scene comparisons and
one complete-warning assertion that failed before comparing its golden file.
The six other failures cover wrapped-editor vertical motion, hardware cursor
position, Help paging/count display, two fixed native-editor comparisons and
the separate Help integration snapshot. A changed body width is not grounds
to remove these behavioral assertions.

Formatting drift was reproduced in 13 files (38 reported regions). Stable
strict Clippy and rustdoc passed; Rust 1.85 strict Clippy reported four existing
diagnostics (`nonminimal_bool`, `unnecessary_map_or`, `comparison_chain`,
`format_collect`), all included in this closeout's fix scope.

Baseline serial mock E2E passed 35/35 on each toolchain using preserved
pre-release-parent Agent binaries. Those runs do **not** establish acceptance
of the exact released pins: final verification requires separate locked builds
from the published commits above, with source and binary hashes recorded.

## Rejected candidate and handoff

An intermediate candidate passed 1187 default tests (54 ignored), 35 exact-release
Agent mock E2Es and nine Release performance tests on each toolchain, plus Linux
kernel-PTY checks. Independent visual review nevertheless rejected its golden
set: the narrowed cancelled-result notice lost the numeric value after
`tool rounds:`. Passing test counts did not justify adopting that regression.

The owning renderer now wraps the full result notice, and W80, narrow-width and
maximum-u64 digit-visibility regressions have been added. The rejected candidate's
results do not validate these changes. At the initial handoff these regressions
were unexecuted, no goldens were adopted, and no closeout commit or push was made.

The layout and exclusive QA members failed with `NATIVE_LENGTH`; a layout revive
failed again. The underlying runtime cause was not established. At the user's
request, that Team closed with a partial outcome and handed the remaining
implementation verification and delivery to a new authorized three-member Team.
No acceptance gate was waived by this administrative closure.

## Fresh candidate review

The new candidate ran on stable Rust 1.97.1. Focused narrow-statistics,
maximum-digit and failure-style regressions each passed (one test each).
Generation and compare-only runs each passed **54 UI tests**, including the
80-column cancelled-result regression; the independent render integration
suite passed **4 tests**. A separately filtered command selected zero tests and
is not counted as evidence.

The fail-closed harness recorded exit 0 for generation, golden-only changes,
compare-only, integration and final source immutability. Its before/after
manifests changed exactly the 62 reviewed golden paths, with no non-golden
changes. Independent review reconstructed each postimage against the candidate
manifest and approved these exact patches, subsequently applied by the lead:

- Remote formatting-only patch SHA-256:
  `3a7d713e6ef55194cc5fa89224b36843e408cd81e88c74b14b81b99b90b7c4bc`.
- Golden adoption patch SHA-256:
  `024f600018fb768f8669888fd411c1dd8616903424a6f7fa9606904dad64e683`.
- Candidate full-file manifest SHA-256:
  `942f91a9158477738164cad918710d3807253a41e73fe151b0f1917fc81459d9`.
- Renderer `src/ui/transcript.rs` SHA-256:
  `786aeec54953329790b76e7ce4037d71db33f9728d76eb07aab1df4c7d94c951`.
- Formatted `src/ui/snapshots.rs` SHA-256:
  `5185074833da877d86874412510ce2ce9ac3e50ecba38050807e961d075c922a`.

The cancelled-result golden now displays `tool rounds:` followed by `0` on
the next body row. UNSAVED warnings retain every character across wrapping.
The remaining differences implement the fixed left gutter, body, right gap and
independent scrollbar column; they do not remove behavioral assertions.
Candidate evidence lives under the builder's
`logs/src-close2-203d49a7/{close2-focused,golden-stable}` within
`/root/minicore-tui-closeout-20261003`, mirrored in
`/tmp/minicore-tui-closeout-qa/close2-candidate`. These focused results authorize
candidate adoption, not final acceptance of all integrated code.

## Final integrated results

The final immutable export `src-close2-final-c70c73c4` passed on the authorized
Linux builder from **2026-10-03 08:40:15 to 08:49:41 UTC**. All driver and gate
exit statuses were zero. This is new evidence for the adopted implementation,
not a substitution of historical phase-F or rejected-candidate results.

| Gate | Stable Rust 1.97.1 | MSRV Rust 1.85.0 |
| --- | --- | --- |
| Locked offline all-target tests | 1189 passed, 0 failed, 54 ignored | 1189 passed, 0 failed, 54 ignored |
| Formatting / strict Clippy / warning-denied rustdoc | All exit 0 | All exit 0 |
| Exact released Agent, serial loopback mock E2E | 35 passed | 35 passed |
| Existing Release performance suite | 9 passed | 9 passed |
| Linux kernel-PTY checks | PASS, 9 cases | PASS, 9 cases |
| Source/binary identity and full-tree immutability | All exit 0 | All exit 0 |

Default totals aggregate 19 test-executable summaries. Ignored cases are not
counted as default passes; E2E and performance results are separate. The PTY
suite includes an intentional negative raw-mode detector, which correctly
reports raw mode rather than restoration. No paid provider was contacted.

The 2166-file full-source manifest SHA-256 is
`c70c73c4bdd017160ff98dc362411d9bf1b0a590c5565a7c7070d457becb60d7`.
The 426-file build/test-input manifest SHA-256 is
`3be66e60b93ec36f5884d76bf3ccd61bdf21a6b611eaf3ff50d96cb7c2532ccb`.
All gate before/after full-tree hashes match. Build inputs cover
`src`, `tests`, `snapshots`, `scripts`, `tools`, `Cargo.toml` and `Cargo.lock`;
subsequent verification-document additions are explicitly outside this manifest.
The final staged build inputs must match it exactly, including path membership.
CI configuration is separately included in the full-source manifest and review.

Fixed Agent binaries report version 0.6.0 and retained their hashes before/after:

- Stable: `854361e303cbafe6bb39492441590087eacd1bee74eabb15ad69e7596607561f`.
- MSRV: `5b003dc09308577c03bbbab4ee85aca3a3c4ffa3d3cd8ee8b6b3dd152a3fc341`.

New TUI 0.3.0 Release-build binaries (test artifacts, **not deployed releases**):

- Stable: `2f433aaca6ca862e5c4ad63c99a6c50beccc6738f81ccaa7c82a593f3e02618a`.
- MSRV: `c0524dd5c8948ca3cecc7f1d7027a4e3561b2f04953cf1646339a7473e7674aa`.

The checked-in `evidence/` directory preserves 61 curated files: manifests,
commands, toolchain/source/backend identities, full quality/E2E logs, performance
logs and PTY JSON. `evidence.sha256` fixes their original contents; its SHA-256 is
`3eea31fd90e4803297d22d94fa6d68149144e67caf21aca3b134ca4688fe5632`.
See `evidence/FINAL-QA-SUMMARY.md` for exact commands, measurements and provenance.
That immutable QA summary describes status at QA completion; delivery updates
belong below rather than rewriting the evidence. Its phrase “released TUI
binaries” refers to the Cargo Release profile, not publication or deployment.

## Delivery

Independent review approved staged tree
`fe87d2e4b7ad6726d179e6c61d010654c9772d67`, including all 169 changed paths,
all 426 exact tested build inputs, all 62 reviewed goldens, and immutable evidence.
The implementation commit is
`93d059815911bcf666a108628f023484fd3920ea`
(`fix(tui): finalize stable page layout and preserve result statistics`).
Its tree is exactly the approved staged tree. Local `dev` fast-forwarded to this
commit from `9d11ee69c4efa02ef1e5bff143662b48dc3194de`; the refactor branch
points to the same implementation commit. A subsequent documentation-only commit
records this delivery outcome without changing any tested input.

The initial non-force atomic push failed with HTTP 403 because the default
`SoPudge` identity had read-only repository access. After the user explicitly
confirmed delivery, the main agent reused the existing `zqcli` identity through
an explicit HTTPS username, without changing credentials, the configured remote
or authentication settings. The non-force atomic push then succeeded.

A fresh `git ls-remote` confirmed both `dev` and
`refactor/v0.3-full-project` at
`11be749f53fb172e27d9bea37d1a3ea5acc396de`. Fetch refreshed both tracking refs;
the index and worktree were clean. This commit contains the reviewed
implementation plus verification-document changes only.

The following hosted runs were independently read to completion on that exact
commit; every run has eight successful jobs:

| Branch | GitHub Actions run | Result |
| --- | --- | --- |
| `dev` | `37112233719` | Completed, success |
| `refactor/v0.3-full-project` | `37112233580` | Completed, success |

Each run covers six native runner test combinations (Ubuntu, macOS and Windows,
with Rust 1.85.0 and stable), the formatting/strict-Clippy/rustdoc/dependency job,
and the exact Agent 0.6.0 / Runtime 0.6.0 loopback E2E job. These are actual
hosted results, separate from the Linux-builder evidence above. Subsequent
verification-document-only updates do not change the 426 tested build inputs.
No tag, version bump, branch deletion, binary deployment or core change occurred.

## Explicit limits

Linux/mock and hosted suites do not establish native iTerm2 interaction,
manual IME or desktop clipboard behavior, real paid-provider behavior, exact
allocator accounting, or terminal input-to-frame latency. These remain separate
acceptance limits unless directly measured with additional authorization.
