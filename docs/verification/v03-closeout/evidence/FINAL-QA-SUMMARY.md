# Final integrated TUI QA — 2026-10-03

## Result

**PASS: the final integrated build/test source passed all authorized Linux-builder quality gates on stable Rust1.97.1 and MSRV Rust1.85.0.** This is new exact-source evidence, not reused previous candidate test counts. Git/staged/remote-ref/hosted-CI delivery remains the lead's work; native manual limits below remain unverified.

Final driver ran from 2026-10-03T08:40:15Z to 2026-10-03T08:49:41Z and exited0. No duplicate driver or competing build was launched after transient model WebSocket failures; the detached finite driver completed normally. All four child gates exited0 and wrote completed.txt. QA had no shared-worktree edits, Git writes, local Rust/Cargo/fmt/tests, core-library changes or paid-provider calls.

## Source provenance

Remote immutable capture: `/root/minicore-tui-closeout-20261003/src-close2-final-c70c73c4`.
Local immutable export: `/tmp/minicore-tui-closeout-qa/close2-final-export/source`.
Full2166-file standard source manifest SHA-256: `c70c73c4bdd017160ff98dc362411d9bf1b0a590c5565a7c7070d457becb60d7`.
Build/test-input426-file manifest SHA-256: `3be66e60b93ec36f5884d76bf3ccd61bdf21a6b611eaf3ff50d96cb7c2532ccb`.
Export tar SHA-256: `78540c88c0f852e97d7b8a5fc4336f85a69392c97a69b4e6974a14fc64c97ddc`.

The426 inputs are all files under src/tests/snapshots/scripts/tools plus Cargo.toml and Cargo.lock. Documentation and CI workflow are retained in the fullmanifest but excluded from the build/test manifest so later verification-record-only changes can be independently mapped without unnecessary Rust reruns. No documentation include_str/build dependency was found in relevant source.

All four remote gate before/after2166-file tree hashes equal fullmanifest c70c73c4; source checksum checks and whole-tree comparisons exited0. This verifies both known-file content and absence of added/removed source files. Source-specific targets are `targets/src-close2-final-c70c73c4-{stable,1.85.0}`. No parentcandidate targets or TUI binaries were reused.

Independent reviewer result:twh4 verified all2166 export/worktree hashes and all426 build inputs; compared reviewed candidate942f91a9 and found only the accurately updated verification README. QA reread all426 current build inputs after final gates: zero mismatches.

Renderer SHA-256: `786aeec54953329790b76e7ce4037d71db33f9728d76eb07aab1df4c7d94c951`.
Formatted snapshots test source SHA-256: `5185074833da877d86874412510ce2ce9ac3e50ecba38050807e961d075c922a`.
Adopted reviewed fmtpatch3a7d713e and normalized goldenpatch024f6000 are reflected in this export. The unknown-cancel W80 golden visibly preserves the trailing tool-rounds numeric0 on its next row; all full-digit/narrow assertions remain meaningful and pass.

## Actual gate results

| Gate | stable1.97.1 | MSRV1.85.0 |
| --- | --- | --- |
| `cargo test --locked --offline --all-targets --no-fail-fast` |1189 passed /0 failed /54 ignored; exit0 |1189 passed /0 failed /54 ignored; exit0 |
| `cargo fmt --all --check` |exit0 |exit0 |
| `cargo clippy --locked --offline --all-targets -- -D warnings` |exit0 |exit0 |
| `RUSTDOCFLAGS='-D warnings' cargo doc --locked --offline --no-deps` |exit0 |exit0 |
| locked offline metadata |exit0 |exit0 |
| exact released real-Agent serial mocked-provider E2E |35 passed /0 failed; exit0 |35 passed /0 failed; exit0 |
| locked offline Release TUI build |exit0 |exit0 |
| existing ignored Release performance suite |9 passed /0 failed; exit0 |9 passed /0 failed; exit0 |
| Linux kernel-PTY report |PASS,9 cases; exit0 |PASS,9 cases; exit0 |
| fullsource/binary identity and immutability checks |all exit0 |all exit0 |

Default1189 counts aggregate19 test-executable summaries. The54 ignored tests are not represented as default passes; separate authorized E2E/performance runs account for35+9 ignored cases. The performance helper also prints a nested1-test summary, which is not added to the9 top-level suite count.

The PTY cases cover terminal enter/restore, editor suspend/resume, raw mode restoration, a deliberate negative raw-mode detector, input/resize/shutdown signal, panic restoration, real TUI input/resize/shutdown,30second idle, and synthetic native-clipboard command roundtrip. All positive restoration cases report cooked terminal after exit; the deliberate negative reports raw as expected.

Stable performance observations: direct256KiB composer p95/p99=217/248us; draft-edit795/886us; C2C120x40 cached workload8676/9015us. MSRV corresponding values213/218us,795/889us,8837/9145us. These are the existing measurements, not exact terminal input-to-frame latency or allocator instrumentation.

## Backend and release binaries

Remote backend repos were readonly-clean and match exactly:
- Agent `d81728b13db68c76b05b4c8cb87161770769947f`, Cargo.lock SHA`47ca2b2e2c11da29194456a5673a5d21a84860073446c7e68556f0963e5edc35`.
- Runtime `9e230617d36130e7ec77aba122b45f1347ac53f2`, Cargo.lock SHA`9575341eab386bdf411a002e5acc576768b1673718b014b23032918a3cc563d2`.

Published fixed Agent binary identity is checked before/after each E2E run, with --version0.6.0:
- stable `854361e303cbafe6bb39492441590087eacd1bee74eabb15ad69e7596607561f`.
- MSRV `5b003dc09308577c03bbbab4ee85aca3a3c4ffa3d3cd8ee8b6b3dd152a3fc341`.

New exactsource released TUI0.3.0 binaries:
- stable `2f433aaca6ca862e5c4ad63c99a6c50beccc6738f81ccaa7c82a593f3e02618a`.
- MSRV `c0524dd5c8948ca3cecc7f1d7027a4e3561b2f04953cf1646339a7473e7674aa`.

PTY test-executable hashes and JSON artifact paths are in perfpty-{toolchain}/test-bins.sha256 and artifact-*.jsonl. The failclosed script checks build exits, single exactartifact existence,9performance count, JSON PASS, fullsource before/after immutability; it does not silently continue after a missing binary.

## Evidence locations and hashes

Remote authoritative logs: `/root/minicore-tui-closeout-20261003/logs/src-close2-final-c70c73c4`.
Local read-only mirror: `/tmp/minicore-tui-closeout-qa/close2-final`.
Mirror archive SHA-256: `c8a3eecd774ca6ca3560d87303ae27b9d3414dd9933bea4de4aa149846f3ba45`.

Local/remote scripts:
- qa-close2-full.sh SHA`b705a526e8a953a5f2c55e48e45dd4c256053b7384c5cae25f489cd241d325e2`.
- qa-close2-perfpty.sh SHA`e92515f3980d516a7841f13bb6a7c49ba6d530c9599794c93ac80ab6a27302d5`.
- qa-close2-driver.sh SHA`109be852c361e068a83f597971c93242f6dda7448170a789d727d397067868a0`.

Gate full logs, statuses, toolchain/binary hashes, source checks, PTY JSON and performance measurements are preserved. Reports/logs contain no SSH password, provider credentials or private configs. Lead may copy the relevant evidence into the verification record and must verify staged426 inputs unchanged. Documentation-only final evidence edits do not change the tested Rust inputs.

## Explicit limits

No native iTerm2/manual IME/desktop clipboard interaction, real paid-provider behavior, exact allocator accounting or terminal input-to-frame latency was measured. Linux PTY/synthetic clipboard/helper tests do not establish those manual gates. Hosted CI and delivered Git refs have not been checked by this QA work. No version/tag/deploy/core release modification is performed.
