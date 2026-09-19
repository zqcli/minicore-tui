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

Current acceptance remains conservative: the current source/test tree is
`9e399d9`, after F-review remediation `daa944a`; `0aa64c5e4d9211351123db059547beddb15c2cce`
is historical core-baseline provenance. Remote Rust 1.85/stable checks each
passed **830 tests, 0 failed, 53 ignored**; the isolated fixed-backend job
passed 34/34 loopback E2Es on both toolchains; and the current Release
performance set passed 9/9 on both. Linux kernel-PTY lifecycle, same-slave
negative raw-mode, input/resize/shutdown, idle, and real clipboard-child
checks passed. Hosted CI, native/manual iTerm2/IME use, exact allocator/RSS
accounting, provider access, and oversized real-Agent items remain unrun where
the environment cannot verify them. Phase F once violated the original
remote-only Rust/Cargo requirement; the older local rustc 1.98.0 record is
retained as excluded provenance, not current acceptance evidence.
See [`docs/refactor-acceptance.md`](docs/refactor-acceptance.md) and
[`docs/verification/v03-f/README.md`](docs/verification/v03-f/README.md).

## Historical releases

The `0.2.x` entries and verification records remain in git history and under
`docs/release-0.2.*`. They describe the earlier Agent 0.3 protocol line and
are not the backend contract for 0.3.0.
