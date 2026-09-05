# Known Limitations

These are deliberate v0.2 boundaries, not hidden fallback behavior:

- External Editor (`Ctrl+G`) is not implemented.
- OSC52 copy of the last assistant response is not implemented.
- Model and reasoning updates are supported through `session.update` and take
  effect at the next model request boundary; the current tool batch is not
  relabeled or interrupted.
- Mid-turn steering is supported via `turn.steer` when a loop is running. Steering queue capacity is managed by the agent (`-32016` indicates a full queue).
- There is no approval UI, compaction control, live Bash/PTY output, MCP, plugin, skill, subagent, session tree, remote Agent, automatic reconnect, or automatic restart.
- Tools run automatically and Bash is not sandboxed; the TUI does not add a
  permission layer.
- Agent events are best-effort. A dropped event sets an event-gap warning and
  the durable transcript is reconciled after `turn.wait`; there is no event
  ACK or replay protocol.
- Crossterm `EventStream` owns its internal event-reading implementation. The
  TUI does not create a separate blocking terminal-reader thread, but it does
  depend on Crossterm's stream behavior.
- Real terminal behavior varies by terminal emulator, platform, Unicode font,
  IME, and PTY. The default tests use `TestBackend`; the real PTY check is
  explicitly ignored.
- Rust verification is intentionally delegated to the remote builder for this
  delivery; no local Cargo build/test command is used in the implementer
  workspace.
- One Agent process must own a `data_dir` at a time because the Agent store
  does not provide a cross-process lock.
- The durable render cache is an update-installed whole-line cache per session,
  not a virtual DOM or visible-block index. It is intentionally simple for
  v0.2 and includes safe fallback rendering when preparation has not completed.
