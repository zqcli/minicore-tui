# Public Reload Correction

Current installed source pair: TUI **`a604e55baf74422722545b86bb9c6d30c31473e6`**
/ Agent **`f1697f78ce48c8f5f3fde0dc9903c153022bfd9e`**. Versions remain **0.2.8 / 0.3.3**;
Runtime remains **0.4.1**, pinned to `6cd2bdbc634437dea925495c61c7eb0be10ba171`.
This is a separately committed, reviewed, built and installed correction. No push,
tag, version bump or new hosted-CI run occurred.

## Why This Correction Exists

The final command-contract audit, after documentation commits TUI `b7d4cc4` and
Agent `3171f42`, found that public `/reload` did not include the old `/refresh`
command's exact `turn.wait` reread. The internal `RefreshTurn` event still worked,
but the old slash-command test had been converted to that internal event. Passing
those tests did not establish public command compatibility. The earlier closeout
missed this requirement; its TUI `30ea7ca` pairing is superseded, not silently
relabeled. The [previous report and raw checksums](../followups/README.md) remain
unchanged as evidence of that earlier scope.

The correction:

- Captures the active Session and retained TurnRef before reload fencing; sends
  `agent.reload` first, then at most one exact `turn.wait`. An unbound new live
  submission never falls back to an older completed result.
- Deduplicates ordinary and reload-origin waits for the same full TurnRef across
  send ACK, explicit refresh and close paths. Different loops remain independent.
- Keeps the result wait outside configuration staging, generation completion and
  reload's stale-read fencing; Session lifecycle invalidation still applies. A running
  wait does not delay completion of configuration reload; the result reread does not
  require a successful configuration ACK.
- Marks this wait's response/send-failure events as reload-origin for same-pass
  FIFO admission. Later ordinary events retain the existing queue rules; original
  User, Tool or model execution is not replayed by the reread.
- Requires current-turn ownership before applying a wait result or failure. A
  late T1 result cannot overwrite settled T2 result/usage, and a late T1 send failure
  cannot clear T2's handoff and cause its queued message to be submitted twice.

No wire method, Agent implementation, Runtime code, dependency or version changed.
Existing rejection/ACK/view-refresh/unknown-outcome distinctions, state versus
History recovery, tombstones, Blocked/Unsaved handling and no implicit retry remain.
The result wait does not recover unsaved history by itself or promise rollback.

## Behavioral Evidence

Ten command/flow regressions were added. Three genuine REDs were observed remotely:

| Gap | Retained log under `logs/revisions/` |
|---|---|
| Public command emitted only `agent.reload`, not the expected exact wait | `parent-reload-refresh-red-valid.log` |
| Late T1 result replaced completed T2 result and its 22/7 token usage | `parent-reload-refresh-red-valid-late_reload_wait_cannot_overwrite_new_completed_turn.log` |
| Late T1 wait send failure cleared T2 handoff | `parent-reload-refresh-red-valid-stale_reload_wait_failure_does_not_clear_t2_handoff.log` |

The additional coverage includes existing-wait deduplication, reload reentry,
response before staging completes, staging completion while the result wait remains
pending, late response followed by an ordinary Tick, failure without retry,
no retained target, and original-Session routing after switching Sessions.

Separate logs preserve test setup problems: unnecessary `str::as_str`, access to
private `App::reload`, use of a moved request, and an extra simulated state query
that the actual handoff did not issue. Compile errors and that driver error are
not counted as product REDs. No old test or snapshot was weakened to achieve GREEN.
A development-service 520 interruption left partial source edits; they were inspected
and continued in the same Luna-max session, not reset or treated as complete.

Independent Luna-max source review approved the corrected wait ownership and
queue semantics after the two races were addressed. This is distinct from the
parent's executed verification and native artifact acceptance below.

## Exact-Source Gates

Fresh Git archives were transferred to **`root@192.168.20.199`**, with local/remote
SHA-256 equality, then built under `/root/minicore-followups.6Dpa6e/reload-refresh`.
All Rust build, test, format, lint and rustdoc gates ran on that authorized builder.
Only downloaded-binary execution, native TTY/iTerm2 work and metadata/hash checks
ran on the Mac. Exact-source logs are under `logs/`; draft/revision logs are separate.

| Gate | Result |
|---|---|
| TUI stable 1.97.1 all-targets | 508 passed / 0 failed / 19 ignored |
| TUI MSRV 1.85.0 all-targets | 508 passed / 0 failed / 19 ignored |
| Real-Agent E2E, stable and MSRV | 18 passed each, explicitly executed |
| Stable fmt, strict Clippy, warning-denied rustdoc, Linux build | PASS |
| TUI macOS Debug/Release, Rust 1.98.0 / Clang-LLD 19 | PASS |
| Mach-O, strict signature, Debug/dSYM UUID | PASS |

Agent `f1697f7` is unchanged: its prior exact-source stable/MSRV result remains
369 passed / 2 ignored. Its Linux binary was freshly built from the fixed archive
for these E2E runs; its already accepted macOS Debug/Release binaries were reused
byte-for-byte. This does not claim a new Agent all-target run or a new Windows gate.
The SDK/minimum-target configuration remains SDK 26.2 / macOS 11.0; execution on
macOS 11 hardware was not newly tested. Historical MSRV strict-Clippy diagnostics
are not claimed fixed or suppressed.

## Native Acceptance

All six workflows were rerun with the corrective TUI and unchanged Agent:
`native/{followups,session,stream}-{debug,release}/result.json`. Their recorded
executable hashes match the accepted candidates.

Feature runs each made **19 actual loopback HTTP model requests** and verified the
existing Tool failure/fold, Codex patch/full-workspace comparison, `/new`, Esc,
stateless single/parallel/chain outputs and strict schema, inherited context,
one parent Store Session with no child records, and idle configuration reload
with unchanged history/no fake User and changed subsequent model requests.
Session runs retained prompt snapshots, busy rename, filtered identity, guarded
close/delete, default-Cancel behavior and shared dock geometry. Stream runs retained
reasoning boundaries, fold/scroll/selection behavior, paced FIFO and Footer state.
Raw glyph/CPU/drag samples are preserved; glyph changes are not FPS and CPU data
is not a matched-baseline performance improvement claim.

The command-level exact-wait and delayed-response races are reducer/RPC-flow
coverage. Native feature acceptance covers the real iTerm2/Agent loopback workflow,
not a new native RPC-wire trace of those forced races, native busy-reload acceptance,
real upstream/TLS behavior or exhaustive pixel parity.

`native/tty/result.json` and raw normal/panic logs record exit **0 / 101**, unchanged
`stty`, all stdio attached to TTYs, and no leaked unflushed-frame marker or destructor
cleanup panic. All test data came from fresh owned absolute temporary directories;
user configuration and conversation stores were not used.

## Installation And Provenance

Only TUI `target/{debug,release}/minicore-tui` and its Debug symbols were replaced.
Both candidates and symbols were staged and checked before separate atomic file
replacements. The old TUI executables were hard-linked and their old inode/hash
verified afterward. Old symbols are preserved alongside them under:
`target/preserved-before-reload-refresh-AdCugX/`.

Agent installation bytes still match the original accepted hashes. Earlier
`preserved-before-followups-AdCugX` backups in both repositories remain intact.
No user process was restarted; an existing process may continue executing an old
image. `/reload` reloads configuration, not executable code. Two file replacements
are not one filesystem transaction.

| Artifact | SHA-256 |
|---|---|
| TUI Debug | `998b10c1a8745116ebdb7254bad43530b74533ce3673f7a4a81cf0d5fc2bce14` |
| TUI Release | `9f61b78ab1ce300bbb714a4b6399a33c05c0ed043e2f78d1e1485fed43a4b203` |
| Agent Debug, unchanged | `443b88b385d778a41303540f940718cf85968a375ca0f334437f9ff42c4703e6` |
| Agent Release, unchanged | `57a1c3722cf05c251f499b8a1e4d50032e9c8b3ce9e4efcd27028ff4dfc73914` |

TUI Debug/dSYM UUID: **`4C4C4459-5555-3144-A1E3-4D9E5D5CD3E3`**.
See `installation.log`, `logs/artifact-verification.log`, `provenance.json` and
`FILES.sha256`. The repository archive includes raw evidence, scripts and logs;
large binaries, symbols and source tarballs live in the separately checksummed
bundle `/tmp/minicore-followups.AdCugX/reload-refresh/`. Its `hashes.txt` paths are
bundle-relative, not assertions that these binaries/tarballs are checked into Git.
Native scripts use the bundled frozen helper. Build/install scripts retain explicit
host paths; this is audit evidence, not a portable SDK/toolchain distribution.

Earlier excluded local-Rust execution, temporary review-output writes and other
historical safety disclosures remain in the previous follow-up reports; this
correction does not erase or reclassify them. Native delegation remains **Stage 1
stateless single/parallel/chain**. Persistent aliases/targets, adopted Sessions,
controls/manager UI and compaction remain unimplemented and outside this correction.
