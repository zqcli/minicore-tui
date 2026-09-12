# TUI 0.2.8 / Agent 0.3.3 Paired Verification

Historical version-update baseline, before the same-version Session-management
follow-up. The original counts and artifacts below are not the current feature
acceptance; see [Session Management Acceptance](../session-management/README.md)
and the current [public reload correction](../reload-refresh/README.md).

All compilation occurred on the authorized Linux builder in
`/root/minicore-release-028-033.Fm9vbA`, reusing owned Cargo caches and the
macOS SDK/LLVM 19 toolchain. No local compilation, real provider access, user
configuration/history inspection or user-process restart occurred.

## Results

| Gate | Result |
|---|---|
| TUI stable all-targets | 423 passed / 17 ignored |
| TUI Rust 1.85 all-targets | 423 passed / 17 ignored |
| Agent stable and Rust 1.85 | 280 passed / 2 ignored each; logs in the Agent repository |
| Real-Agent loopback E2E | 16/16 passed |
| Both repositories fmt / Clippy / rustdoc | Passed, warnings denied where applicable |
| Four macOS Debug/Release executables | Rust 1.98, Clang/LLD 19 cross-builds passed |
| Native Debug and Release pairs | TUI version 0.2.8; `agent.ping` version 0.3.3; interaction checks passed |
| Real TTY normal/panic | Exit 0 / 101; all stdio TTY; unchanged stty; pending marker absent |
| Installed artifacts | Byte-identical to tested files; signatures and Debug/dSYM UUIDs checked |
| Local/remote source hashes | Identical for each repository |

The native driver covers reasoning boundaries, Steer FIFO and withdrawal, narrow
layout, acknowledged Footer reasoning, busy responsiveness, long-history
hover/wheel/drag and normal shell restoration. Captures are native screen text,
not verified pixel screenshots. Loopback-only tests do not certify a real upstream
route or TLS handshake. The macOS 11 deployment header is checked, not execution
on macOS 11. New GitHub Actions platform results are separate from these gates.

## Files

- `remote/`: final TUI logs, E2E and the exact remote build script. Agent logs are
  in its `docs/verification/0.3.3/remote/`.
- `iterm/debug/`, `iterm/release/`: final native results, current-screen captures,
  owned-process samples and scroll timing. CPU output is retained diagnostic data,
  not a new performance claim for this version-only TUI patch.
- `iterm/driver.py`, `iterm/tty_check.py`: native procedures. Use
  `MINICORE_VERIFY_TUI` and `MINICORE_VERIFY_AGENT` to select a matching pair.
- `iterm/native-pty-final-*`: real-TTY results.
- `iterm/native-debug-harness-failure.log`: a failed first run caused by the
  added ping probe overwriting the mock server's `requests` list. Renaming the
  probe variable fixed the harness; product source was not changed. The owned
  failed-run TUI also exited zero before its window was closed.
- `installed-builds.txt`, `agent-builds.txt`: new/retained executable hashes and UUIDs.
- `source-local.txt`, `source-remote.txt`: exact Rust source equality evidence.

Previous executables and symbols are retained in each repository's
`target/preserved-before-release-ew5WEt/`. For LLDB source lookup, map
`/root/minicore-release-028-033.Fm9vbA/minicore-tui` to the local TUI checkout;
map the corresponding Agent path for Agent debugging. Debug optimization and
remaining Agent presentation-path limitations are documented in release notes.
