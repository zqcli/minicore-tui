# TUI 0.2.4 / Agent 0.3.2 Verification

Local macOS verification of the uncommitted build. Runtime source and the
fixed Rail/Pi reference fixtures were not modified. No commit or release tag
was created. These results do not relabel earlier cross-platform evidence.

## Final Gates

| Gate | Result | Log |
| --- | --- | --- |
| TUI stable, all targets | 401 passed / 17 default-ignored | `tui-stable.log` |
| TUI Rust 1.85.0, all targets | 401 passed / 17 default-ignored | `tui-msrv.log` |
| Agent stable, all targets | 279 passed / 2 ignored | `agent-stable.log` |
| Agent Rust 1.85.0, all targets | 279 passed / 2 ignored | `agent-msrv.log` |
| Real-Agent loopback E2E | 16 passed, explicitly enabled, sequential | `e2e.log` |
| Real iTerm2 TTY enter/restore | 1 passed; stdin/stdout verified as terminals, not skipped | `iterm/native-pty.log` |
| Formatting, all-target Clippy, docs | Both repos passed, warnings denied for Clippy/docs | Per-repo logs |
| Debug and Release builds | TUI 0.2.4 / Agent 0.3.2 | Build logs and `builds.txt` |

All test commands used locked/offline dependencies. The 17 default-ignored
TUI tests are the 16 E2E cases and the one real-TTY test, all run separately.
The complete 12-case E2E baseline remains, including the same-loop model update.
New E2E cases cover reasoning boundaries, client-paced steering, duplicate
steer text, and the unchanged direct-RPC batching contract.

## Native iTerm2

The one-off `iterm/driver.py` ran from iTerm2 and controlled a separate real
**iTerm2 3.6.11** session running the actual TUI and Agent executables. All
configuration, Agent storage, and bash effects were in a fresh absolute system
temporary directory. No user config or Store was loaded; the only provider
was a synthetic loopback server.

`iterm/result.json` records a PASS with unchanged TUI/Agent executable hashes:

1. SSE fragments `Plan`, `ning`, ` phase` formed `Planning phase`, including
   a fragment without identity metadata. Two later `summary_index` values
   contained no raw newlines but appeared on distinct native terminal rows.
2. One rapid input sequence submitted Alpha and Beta. Both appeared in the
   gray dock queue above Working, separated from it by a blank row. Neither
   pending item appeared as a User card.
3. Option+Up withdrew the next unsent Beta into the empty editor. Re-submission
   changed its local identity but did not lose or duplicate either message.
4. The 60×16 checkpoint retained both queue rows, the editing hint, Working,
   the editor, and the one-row Footer. Low-priority Footer text may truncate;
   the native assertion checks the actual queue rows, not a hidden counter.
5. The Alpha model request contained Alpha and not Beta. Only the subsequent
   request contained Beta and the preceding Alpha answer. Both final User
   entries appeared once; the queue was absent from the completed screen.
6. Selecting `max` after completion updated the Footer without another turn.
7. The native TUI exited zero and restored its shell. A separate compiled
   real-TTY test subsequently entered/restored the alternate screen and passed.

Current-screen text is restricted to the session's current row count. iTerm
`contents` can include resize scrollback; old queue rows in that scrollback
are not treated as current queue state. Checkpoints cover 100×32 and 60×16.

## Evidence Limits

No native pixel screenshot is claimed. iTerm's protected/non-shared windows
prevented reliable programmatic capture in the preceding investigation; this
run saved native screen text and made no screenshot attempt or reconstructed
replacement. Text/input evidence is not font/color/pixel parity evidence.

The provider is deterministic test data, not the user's upstream service.
Steering pacing means one new queued instruction per issued model request,
not a promise to finish every multi-request task before accepting a redirect.
Uncertain delivery remains held rather than retried. Existing saved reasoning
with already-lost boundaries is not guessed or rewritten.

`builds.txt`, `tui-source-hashes.txt`, and `agent-source-hashes.txt` identify the
final artifacts and source inputs. Previous release evidence remains historical.
