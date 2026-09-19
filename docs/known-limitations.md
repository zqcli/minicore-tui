# Known Limitations

These are the explicit 0.3.0 boundaries, not hidden fallback behavior:

- The default history decode path does not automatically decode items above
  8 MiB. They remain explicit placeholders; `/export raw` is a separate,
  bounded path and does not raise the automatic decode ceiling.
- Exact allocator/RSS accounting, decode-throughput qualification, terminal
  input-to-frame latency, and a real external provider are not claimed.
- The final source baseline has local Rust evidence only until the authorized
  remote Rust 1.85/stable and fixed-Agent runs complete. Native macOS/Windows
  execution, hosted CI, interactive iTerm2/IME/clipboard behavior, and the
  real-PTY editor round trip remain environment-dependent acceptance checks
  until they are run.
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
