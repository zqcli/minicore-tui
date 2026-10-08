# Known Limitations

These are the explicit 0.3.0 boundaries, not hidden fallback behavior:

- The default history decode path does not automatically decode items above
  8 MiB. They remain explicit placeholders; `/export raw` is a separate,
  bounded path and does not raise the automatic decode ceiling.
- Exact allocator/RSS accounting, decode-throughput qualification, terminal
  input-to-frame latency, and a real external provider are not claimed. The
  exact Linux OS clipboard-helper cancellation and paused-Agent/large-stdout
  Spec §25 scenarios are covered; that evidence does not imply native desktop
  or hosted CI behavior.
- Current Rust evidence is from the authorized remote Linux builder: Rust
  1.85.0 and stable each pass 830 tests with 53 ignored, and the fixed-Agent
  loopback job passes 34/34. Native macOS/Windows execution, hosted CI,
  interactive iTerm2/IME/clipboard behavior, and external-provider access
  remain environment-dependent acceptance checks. Linux kernel-PTY evidence
  is recorded separately and does not imply native/manual terminal acceptance.
- There is no approval UI, MCP, plugin, skill, persistent subagent manager,
  session tree/fork/branch, automatic reconnect/restart, full Bash/PTY output,
  OSC52 copy, Git mutation, or patch export.
- Tools run automatically under the Agent and Bash is not sandboxed; the TUI
  does not add a permission layer.
- Agent events are best-effort. A dropped event sets an event-gap warning and
  authoritative `turn.wait`, `session.state`, and `session.read` reads reconcile
  the view; the TUI does not ACK or replay events.
- The TUI does not read Agent config/data or workspace content directly. Provider
  credentials, catalogs, models, tools, and `data_dir` remain Agent-owned, and
  one Agent process must own a `data_dir` at a time.
- The durable render cache is an update-installed owner-level cache rather than
  a virtual DOM or full terminal-cell cache. It uses bounded layout/history
  budgets and safe fallback rendering while asynchronous preparation is pending.
- Optional read/edit/write argument previews are best-effort, unvalidated,
  bounded snapshots. Generated arguments do not imply execution. Missing or
  truncated content cannot be recovered by expanding the preview. Older
  Agents remain usable without this optional feature. This implementation
  does not calculate speculative edit diffs or introduce execution output
  streaming for read/edit/write.
