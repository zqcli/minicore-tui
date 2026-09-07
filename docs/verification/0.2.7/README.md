# 0.2.7 Verification

Authoritative evidence for the scrolling/output patch. All compilation was on
`root@192.168.20.199` in `/root/minicore-tui-027-RXdxEP`, using isolated source,
workspace and stores. No real upstream request, user configuration/history read,
local build, or user-process restart was used for this patch.

See [release notes](../../release-0.2.7.md) for changes, measurements and limits.

## Evidence Map

| Path | Meaning |
|---|---|
| `remote/stable.log`, `remote/msrv.log` | Final level-1 package build: 423 passed / 17 ignored each |
| `remote/e2e.log` | 16/16 separate real-Agent loopback E2E |
| `remote/fmt.log`, `clippy.log`, `doc.log` | Final formatting/lint/docs gates; empty fmt log means success |
| `remote/macos-llvm-*.log` | Final macOS baseline, Debug/Release and test builds |
| `remote/canonical-macos-msrv-build.log` | Tracked Rust 1.85 macOS build script succeeded |
| `remote/remote-checks.sh` | Exact isolated Linux build/test procedure |
| `remote/remote-macos-llvm-build.sh` | Matched Rust 1.98 macOS artifacts and remote dSYM generation |
| `remote/remote-opt1-build.sh` | Package-opt1 experiment, before default configuration changed |
| `remote/writer-red.log`, `writer-green.log` | Initial pass-through red and buffered green tests |
| `remote/fixture-race-full.log`, `fixture-red.log` | Real directory race and deterministic ownership regression before fix |
| `iterm/fixed-baseline0` | Matched 0.2.6 Debug; primary CPU baseline |
| `iterm/fixed-buffered0` | Buffer only: no meaningful opt0 CPU improvement |
| `iterm/fixed-baseline1` | Old source with package level 1 |
| `iterm/fixed-buffered1` | Combined level 1 + buffer experiment |
| `iterm/final-debug`, `iterm/final-release` | Final installed-artifact native measurements and interaction acceptance |
| `iterm/driver.py` | Final absolute-25Hz injection, recorded timestamps, CPU windows and sample procedure |
| `iterm/native-pty-final-*`, `tty_check.py` | Final real-TTY normal/panic exit, stty and pending-frame checks |
| `iterm/final-native-*` | Final native writer and isolated-fixture unit test results |
| `iterm/before-debug`, `iterm/llvm-*` | Earlier variable-rate diagnostics; not comparable headline CPU numbers |
| `diagnostics/` | Rejected Zig-linked native unwind failures and standalone probe source; LLVM probe success |
| `remote/macos-*.log`, `remote/probe-*.log` | Includes rejected Zig builds and intermediate link failures; distinguish `macos-llvm-*` |
| `compiler-profiles.tsv` | Cargo-reported package/dependency opt level, debug info and assertions |
| `toolchain.txt` | Compiler/linker versions |
| `installed-builds.txt`, `binaries-before.txt` | Installed and preserved executable hashes; Agent unchanged; Debug/dSYM UUIDs |
| `final-macos-verification.log` | Mach-O, signatures, minos header and parser self-tests |
| `source/` | Before/after Rust hashes and expected four-file scope difference |
| `cleanup/` | Explicit cleanup procedure, retained hashes and deletion totals; do not rerun blindly |

## Measurement Rules

The actual terminal was 220×53, not the requested 220×55. Each scroll run injected
800 wheel events using absolute 40ms deadlines, reversing every 80 events.
`scroll-timing.json` records timestamps. Individual CPU windows saw 198–202 events
in final runs; the full sequence remained 800 events in about 31.96 seconds.
Each `result.json` retains wall time, process CPU increments, event counts and
current-screen checks. Scrolling must visibly move and input must drain.

The three 8-second windows do not overlap `sample`. The optional 800-event total
includes the later 3-second sample and drain/check overhead, so use the primary
windows for CPU claims. Process CPU percentages are per logical core. `ps time`
has 0.01-second resolution; a zero increment is below resolution, not zero work.
Old/new builds share Rust 1.98, Clang/LLD 19 and SDK. Do not attribute the original
local-binary versus cross-build difference to buffering.

The first buffer-only experiment showed fewer mock writes but no clear CPU gain.
At package level 1, the buffer provided an additional gain. Both effects and the
variation between prototype and final runs remain visible in the raw evidence.
These are local controlled runs, not broad statistical or streaming guarantees.
All models/workspaces are synthetic and loopback-only. Samples target owned test
processes only. All driver-owned windows/processes exited normally on final runs.
Native text/interaction evidence is not a native pixel screenshot.

## Scope

Of the 59 original Rust source/test files, 55 retain their hashes. Expected
changes are `src/app.rs` (two MSRV syntax equivalents), `src/terminal.rs` (buffered
writer/lifecycle), `src/rpc.rs` (test-only fixture ownership), and
`tests/terminal_restore.rs` (pending-frame probes). One new file,
`src/terminal/writer_tests.rs`, adds 10 tests. The FAILED lines in
`source/source-scope-check.log` are these expected hash differences, not test failures.
Cargo version/profile/features, 11 version-bearing snapshots, macOS scripts and
documentation are also changed. No dependency version or Agent/Runtime code changed.

The canonical cross-build helper uses Rust 1.85; final installed artifacts and
CPU controls use Rust 1.98 to match compiler versions. Clang/LLD 19 replaces the
rejected Zig 0.13 path. A standalone Zig-linked panic/catch probe crashed with
Rust 1.85 and 1.98; the exact internal cause was not diagnosed. Final native panic
restoration passes. The minimum-OS load command is checked, not native macOS 11.
