# Changelog

## 0.3.0 — Protocol v1 refactor

The 0.3.0 package targets the fixed `minicore-agent` 0.5.0 and
`minicore-runtime` 0.4.1 revisions over Protocol v1.

- Make paged `session.read` the sole application history path; retain
  `session.history` only for compatibility/diagnostics.
- Add bounded chunk assembly, byte-accurate cursors, pinned history windows,
  exact turn-result recovery, lifecycle generations, and explicit incomplete or
  oversized-history states.
- Add bounded Tool detail, workspace file/search/preview, Changes/Diff, and
  Context views with query-slot, epoch, cursor, and raw-byte validation.
- Add bounded full-session search, copy, and export workflows with explicit
  partial/uncertain labels, atomic no-clobber commits, and cancellation that
  waits for the owned writer outcome.
- Add local TOML settings, CLI-over-config precedence, settings persistence,
  and a direct external-editor workflow that suspends only terminal input and
  drawing while RPC, reducer, and background work continue.
- Keep provider credentials/catalogs and Agent data outside TUI settings and
  outside logs; Agent path changes apply on the next startup.
- Keep query ownership separate from queued intent across close/reopen/delete;
  coalesce reopened Tool details, reclaim `turn.result` retry slots, and retire
  never-written session retries at lifecycle boundaries.
- Add offline-capable Rust 1.85/stable CI coverage on Ubuntu, macOS, and
  Windows, plus a separately pinned Agent/Runtime build and loopback E2E job.

Current acceptance remains conservative: the current code/test/snapshot baseline
is `0aa64c5e4d9211351123db059547beddb15c2cce`; local rustc 1.98.0 validation
passed 828 tests with no failures and 43 ignored, and the release performance
suite passed 6/6. The final-source remote Rust 1.85/stable checks, fixed
Agent/Runtime builds, and 34/34 pinned-Agent E2Es passed on both toolchains.
Hosted CI, native/manual terminal interaction, exact RSS accounting, provider
access, and oversized real-Agent items remain unrun where the environment
cannot verify them. See
[`docs/refactor-acceptance.md`](docs/refactor-acceptance.md).

## Historical releases

The `0.2.x` entries and verification records remain in git history and under
`docs/release-0.2.*`. They describe the earlier Agent 0.3 protocol line and
are not the backend contract for 0.3.0.
