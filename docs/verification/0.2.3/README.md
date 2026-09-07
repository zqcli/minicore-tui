# 0.2.3 Local Verification

This records the uncommitted macOS follow-up for Footer settings, accepted
Steer cards, Thinking copy decoration, and the preceding 0.2.2 spacing and
extended-reasoning fixes. It is not a new cross-platform or complete Rail-parity
claim. The fixed Rail/Pi source fixtures were not changed.

## Automated Gates

| Gate | Result | Evidence |
| --- | --- | --- |
| Stable, locked offline, all targets | 378 passed / 13 default-ignored | `stable.log` |
| Rust 1.85.0, locked offline, all targets | 378 passed / 13 default-ignored | `msrv.log` |
| Real-Agent loopback E2E, explicitly enabled | 12 passed | `e2e.log` |
| Agent all-target tests | 267 passed / 2 ignored | `agent.log` |
| Formatting / all-target Clippy / docs | Passed, warnings denied for Clippy/docs | `fmt.log`, `clippy.log`, `doc.log` |
| Real TTY enter/restore in iTerm2 | 1 passed; no non-TTY skip | `iterm/terminal-restore.txt` |
| Debug and Release builds | Both report 0.2.3 | `builds.txt`, build logs |

The 13 default-ignored TUI tests are the 12 E2E scenarios and the one real-TTY
round trip, all executed separately here. Use `rustup run 1.85.0 cargo ...`
when the installed `cargo` is not a rustup proxy accepting `+1.85.0`.

`e2e-initial-failure.log` preserves the initial failure before the test-only
socket-mode and asynchronous-History waiting fixes. Production RPC behavior
was not modified by those fixes. `source-hashes.txt` identifies the final TUI
source/test inputs; `builds.txt` identifies the rebuilt executables.

## Native iTerm2 Checks

`iterm/driver.py` was launched from iTerm2 and controlled a separate real
**iTerm2 3.6.11** session. It ran the actual TUI and Agent against a loopback
Responses provider. All Agent data, bash effects, and configuration were in a
fresh absolute temporary directory, never a user session directory.

The native screen-text and request assertions in `iterm/result.json` passed:

- Idle reasoning changed to `max` before any provider request.
- A real bash tool executed under `approval = "auto"`.
- While bash was running, the Footer changed immediately to acknowledged
  `high`, while the already-started request retained literal `max`.
- An accepted Steer appeared as a User card before the tool was released.
- The next request used `high` and included the Steer; history reconciliation
  left exactly one final card, without the pending marker.
- Idle `/reasoning` and `/model` changes with existing history updated the
  Footer without sending another turn.
- The TUI exited zero and returned to the native shell. Both binary hashes
  were unchanged throughout the run.

Native text checkpoints cover 100×32, 80×24, and 60×16. There were no calls to
the user's upstream model service; provider payloads were synthetic test data.

## Screenshot Limitation

**No native pixel screenshot is verified for this patch.** Although the screen
capture preflight returned true, iTerm windows were reported on-screen with
`kCGWindowSharingState = 0`. Captured PNGs contained only the desktop. They were
visually rejected and are intentionally absent from this evidence directory.
No window-sharing/privacy setting was changed, and no reconstructed image was
substituted. `result.json`'s PASS concerns native interaction, screen text, and
protocol assertions, not the screenshot attempt. See `iterm/validation.json`.

Native screen text does not prove font/color/pixel parity. The older r5 and
xterm.js artifacts remain historical evidence, not new 0.2.3 screenshots.
