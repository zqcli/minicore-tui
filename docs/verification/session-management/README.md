# Session Management Acceptance

Final source and native acceptance of Session management, shared dock panels,
and the paired Agent rename/prompt-file implementation. This report supersedes
the stage-local safety conclusions in the Stage 3/4 reports. Versions remain
TUI **0.2.8**, Agent **0.3.3**, Runtime **0.4.1** at
`6cd2bdbc634437dea925495c61c7eb0be10ba171`. No dependency changes, release tags,
pushes, or new hosted-CI results are part of this follow-up.

## Source And Gates

The parent built fresh `git archive` inputs, not the installed binaries or a
mutable worktree: TUI `cc7675728fdc87754a495cec7db889a1a5074ada` and Agent
`22ecb3df5fe20ddc8fa37c7fb74aa0cc21f6e416`. Source and corrective changes were
committed separately as soon as their review/test boundary was complete:

| Repository | Commit | Boundary |
|---|---|---|
| TUI | `d8ba891` | Streaming folds, absolute ticks, shared drag/body geometry |
| Agent | `ebd0ef7` | Metadata-only `session.rename` |
| Agent | `22ecb3d` | Explicit local prompt files and session snapshots |
| TUI | `a6894bf` | Session operations and shared dock-panel layout |
| TUI | `086fa84` | Action selection must belong to the filtered list |
| TUI | `cc76757` | Freeze dialog targets; reconcile on every return to Browse |

All compilation, formatting and Rust tests ran on the authorized Linux builder.
macOS only executed/inspected downloaded artifacts. The two independent Luna-max
implementation/review sessions approved the final source; parent verification
then used `/root/minicore-session-panels.H0sLzv/native-final/`.

| Gate | Result | Archived evidence |
|---|---|---|
| TUI stable / Rust 1.85 all-targets | **464 passed, 18 ignored**, each | `logs/parent/minicore-tui-{stable,msrv}.log` |
| Agent stable / Rust 1.85 all-targets | **318 passed, 2 ignored**, each | `logs/parent/minicore-agent-{stable,msrv}.log` |
| Real Agent E2E | **17 passed** on both toolchains in revision verification; parent repeated stable | `logs/revisions/panel-cancel-filter-fix-e2e.log`, `logs/parent/e2e.log` |
| Stable strict Clippy, format, warning-denied rustdoc | Both repositories passed | `logs/parent/*-{clippy,fmt,doc}.log` |
| TUI stable/MSRV fmt and rustdoc | Passed | `logs/revisions/panel-cancel-filter-fix-quality.log` |
| Native macOS Debug / Release | Session and streaming flows passed | `native/{session,stream}-{debug,release}/result.json` |
| Real iTerm2/PTTY normal / panic | Exit **0 / 101**, unchanged `stty`, unflushed frame absent | `native/native-pty-final-result.json` |
| Mach-O, signature, symbols | All four artifacts passed; Debug/dSYM UUIDs match | `logs/parent/*-macho-*.log`, `installation.log` |

All-targets uses Cargo's default test threading; the opt-in real-Agent E2E suite
runs serially. The 18 default TUI ignores are 17 real-Agent cases plus one real
PTY case, subsequently exercised explicitly. MSRV strict Clippy is **not**
claimed green: accepted pre-existing diagnostics remain in TUI
`editor_layout.rs`, `rail.rs`, `markdown.rs`, and Agent `presentation.rs`.
These were neither suppressed nor changed as part of this work.

## Behavior And Safety

Session browser operations are Open, New, Refresh, Rename, Close and Delete.
Rename is ACK-driven and does not update execution configuration. Close and
Delete consult authoritative lifecycle/history evidence: unknown state,
pending reconciliation, event gaps, blocked/unsaved and unconfirmed results
cannot become implicit destructive permission. Existing explicit force-close
semantics remain separate and do not manufacture permission to delete.

History requests capture the current gap revision at issuance; an older reply
cannot clear a newer gap. A successful deletion tombstones the ID and retires
only that session's requests, preventing delayed open/rename/state/history and
event responses from resurrecting it. Tests exercise both panel and slash paths.

The shared panel module owns frame/title/query/body/footer/window geometry for
Session, Model, Profile, Reasoning, New Session, Help and Logs. Component tests
cover keyboard paging, pointer targets, narrow layouts and transcript isolation.
Native tests exercise real mouse-input sequences through iTerm2, not OS pointer
tracking or screenshot pixels.

The final native Session flow, separately repeated in Debug and Release:

- Creates only synthetic sessions in fresh absolute temporary directories.
- Loads a relative Markdown prompt from a config directory different from cwd;
  edits the file after startup and verifies all three actual loopback requests
  retain the original prompt, including a session created after that edit.
- Renames a busy session with mixed Latin/Chinese text using the mouse action;
  attempting Delete does not close/cancel its live request.
- Creates another active session, filters to the original non-active one, and
  confirms F5 keeps its visible selected identity.
- Closes that selected session, then separately shows permanent Delete with
  target title/short ID, warning, Cancel and Delete visible at **60×16**.
- Confirms default Enter cancels and a fresh list still contains the session;
  explicitly clicks Delete and verifies the other session survives.
- Selects model/reasoning with mouse input, visits Profile/New/Help/Logs, sends
  another turn using the acknowledged model, and exits with code 0.

The final native streaming regression also retains Tool click folding, distinct
reasoning-part rows, receipt-paced Alpha/Beta FIFO, immediate Footer settings,
and drag/body synchronization. Debug/Release observed **43/40** glyph changes
under roughly two seconds of RPC flood, and **8/9** distinct body windows in
12 drag samples. These are screen observations, **not exact FPS**. Raw CPU,
timestamps and all samples remain archived; this is a regression check, not a
new matched performance-improvement claim.

## Defects Found During Acceptance

The first native Session attempt using source `a6894bf` exposed a real product
bug: filtering changed the rendered rows but retained the hidden active
SessionId. The Close action therefore closed the wrong **synthetic** session.
The test stopped when the subsequent Delete confirmation showed the wrong
target; it did **not** permanently delete it. This failure is retained in
`native/failures/narrow-confirm/` (the old harness label was misleading).

`086fa84` fixes filtered selection. Subsequent review found that refresh during
Rename/confirmation must not retarget its dialog, while ACK/Cancel/Esc must
reconcile after returning to Browse. `cc76757` fixes those transitions and adds
behavioral RED/GREEN cases. Representative final tests are:

- `session_selector_query_refresh_and_footer_actions_keep_filtered_target`
- `session_rename_dialog_freezes_target_and_reconciles_before_footer_actions`
- `session_rename_ack_clears_hidden_target_when_query_has_no_matches`
- `session_panel_cancel_reconciles_after_frozen_target_updates`

`logs/revisions/` preserves the REDs, revisions and final gates. Some earlier
focused commands selected **zero tests**; their success is not coverage. Final
full-suite results and the explicit nonzero RED/GREEN cases establish coverage.
Compile/fixture setup failures are likewise not behavioral REDs.

Other native failures were harness issues, retained separately: an interactive
shell startup prompt consumed the launch command; a synthetic Responses event
omitted required usage details; consecutive Esc inputs failed to return through
two panels; the Logs title assertion expected `Logs` rather than `Agent logs`.
The final scripts use an owned `/bin/sh` window, the repository's validated
completion-event helper, and observed panel transitions. Product checks were
not weakened. An initial parent source mirror included AppleDouble fixture
files; it was preserved as `final/source/minicore-tui-with-appledouble` remotely.
Final archive-based builds contain no such metadata files.

## Installation And Limits

All four natively accepted artifacts were installed at the repositories' existing
`target/debug/<project>` and `target/release/<project>` paths. The staged
replacement preserved old executable inodes and dSYMs under each repository's
`target/preserved-before-session-M0rGv9/`. `installation.log` records old/new
hashes, inode checks and matching Debug symbols; `logs/parent/hashes.txt` records
the remote artifact hashes. Installed files were byte-compared with the accepted
artifacts. User configuration and Store data were not modified, and existing
user processes were not restarted. They may therefore still execute an older image.

Native evidence is **real iTerm2 screen text/input and real TTY restoration**.
Pixel screenshots remain **UNVERIFIED**. There is no claim of current Windows
CI, macOS 11 hardware execution, real-upstream TLS, or deployed-proxy reasoning
behavior. Live Assistant Markdown and broader long-history live-copy work remain
deferred; no `max` remapping was introduced.

The archived scripts are exact one-off verification inputs with absolute scratch
paths, not a new configurable test framework. They must only target fresh owned
fixtures. Raw synthetic screen text intentionally preserves trailing spaces.
