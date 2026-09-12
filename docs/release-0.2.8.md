# MiniCore TUI 0.2.8

This is the historical version-update baseline, before the same-version
[Session-management follow-up](verification/session-management/README.md).
The follow-up has separate commits, tests and installed artifacts, and has not
been pushed. The later [Tool/reload/stateless subagent follow-up](verification/followups/README.md)
also retains these versions with separate commits and installed artifacts.
Statements below concern only the original paired release.

Paired version update with MiniCore Agent 0.3.3. The Rail UI and performance work
was committed as `c06f3d3`; this patch updates the package version, lockfile,
eleven startup-version snapshots and current release documentation.

There are no additional TUI Rust-source, rendering, scheduling, RPC, selection,
Steer or Markdown changes relative to 0.2.7. Debug remains package opt-level 1
and dependency level 2, with debug information/assertions and the documented
optimized-debugging tradeoffs. The 64 KiB terminal writer and the 30 FPS/100 ms
render/tick settings are unchanged.

Agent 0.3.3 commits the existing presentation, reasoning-summary and Steer-receipt
work; the previously identified presentation/storage coupling, unbounded branch
query and whole-history result scan remain known limitations. This version
update does not claim to fix them. Runtime's pinned revision is unchanged.

All new compilation is performed on the authorized remote Linux builder. macOS
only executes, verifies and installs cross-built artifacts. Previous executable
files are retained during installation, and user processes are not restarted.

See [verification/0.2.8](verification/0.2.8/README.md) for paired release checks.
The [0.2.7 performance results](release-0.2.7.md) remain historical measurements;
this version bump does not claim another performance improvement. The original
paired-release source changes were pushed on `dev` without creating a release tag.
