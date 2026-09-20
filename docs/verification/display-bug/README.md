# Display Bug Verification

This evidence validates the async transcript-section fix from `b1bb062`, tool-section deduplication, cached tool ownership from `311c5b3`, and history-only tool marker recovery from `d960c8f`. Validation uses the real TUI and fixed Agent, a deterministic loopback HTTP Responses model, Linux and native Intel macOS kernel PTYs, and `pyte` terminal parsing. All Rust compilation was remote; local checks only executed downloaded binaries.

## Reproduce

The harness needs only Python and the pure-Python `pyte` package; it does not compile Rust locally and does not contact an external Provider.

```sh
python3 -m venv /tmp/mctui-display-pyte-venv
/tmp/mctui-display-pyte-venv/bin/python -m pip install pyte

/tmp/mctui-display-pyte-venv/bin/python scripts/display_conversation_validation.py \
  --tui-bin /path/to/fixed/minicore-tui \
  --old-tui-bin /path/to/minicore-tui-built-from-7fea27e \
  --agent-bin /path/to/fixed/minicore-agent \
  --output /tmp/display-conversation-evidence
```

For the accepted remote run, the command was executed on `root@192.168.20.199` with:

```text
TUI:   /root/minicore-tui-v03-refactor/tui-target/display-bug-stateless-recovery/release/minicore-tui
OLD:   /root/minicore-tui-v03-refactor/tui-target/display-bug-old-7fea27e/debug/minicore-tui
Agent: /root/minicore-tui-v03-refactor/fixedagent-target/debug/minicore-agent
OUT:   /root/minicore-tui-v03-refactor/logs/display-bug/conversation-validation-stateless-recovery
```

`pyte.ByteStream` handles UTF-8 and display-cell width, while the harness explicitly maintains primary and alternate buffers for `CSI ?1047/1049 h/l`. The PTY is created with `pty.fork`, `setsid`, and `TIOCSWINSZ`; the harness never reads a pipe or reconstructs the screen from raw substring matching.

## Covered Cases

The fixed run uses one isolated workspace and Agent data directory, and one loopback server:

1. ASCII ordinary turn: streaming prefix is observed before the completed response, and the completed response remains visible.
2. Chinese ordinary turn: `中文普通消息-唯一` and `助手中文-回复-唯一` are rendered with CJK cell width handling.
3. Tool turn: `tool-before-unique` appears before the tool result and `tool-after-unique` appears after it.
4. The TUI exits, then restarts with `--continue` against the same temporary workspace/data directory; the complete history is restored from the Agent store.
5. The final screen requires every User/Assistant/pre-tool/post-tool message marker exactly once and in order. It also requires the tool result to appear between the pre- and post-tool text.
6. A binary built from the original `7fea27e` source is run through the same tool scenario. It must omit `tool-after-unique`; this is the expected negative control for the original defect.

The loopback request log contains four model calls: ASCII, Chinese, tool-call, and post-tool completion. No API key or external Provider is used.

## Tool ownership transition

The durable layout builds its ToolKey index once with the immutable `ConversationLayout`; cached live-tail composition borrows that index and does not scan transcript history. When a partial Assistant ToolCall lacks a matching durable ToolBlock, a real live tool retains ownership of its current result/status until the ToolResult arrives. If no live owner exists (for example, browsing a partial history window), a fallback card preserves the call and available ToolFacts. The bounded live-owner set participates in layout cache identity so appearing or disappearing live tools cannot reuse a stale fallback layout. Focused regressions cover these transitions, nonzero viewport ordering, and cached ToolKey-index reuse.

## Accepted Remote Result

`PROVENANCE.json` records binary hashes and all assertions:

```text
/root/minicore-tui-v03-refactor/logs/display-bug/conversation-validation-stateless-recovery/PROVENANCE.json
```

Important results:

```text
fixed first exit:       0
fixed --continue exit:  0
streaming ASCII:        true
fixed message markers:  all 1, ordered
tool result count:      exactly 1 on first and --continue screens
fixed responses:        requests 1..4 each completed
old 7fea27e requests:    1..4 each completed, ready observed
old 7fea27e marker:     tool-after-unique missing (expected)
```

The fixed first and resumed screens each contain exactly one tool-result presentation. The harness treats a tool-result count other than `1` as a failure, while the User/Assistant/pre-tool/post-tool message assertions remain strict.

Artifacts include:

```text
fixed/streaming-ascii.screen.txt
fixed/final.screen.txt
fixed/continue.screen.txt
fixed/first.pty.raw
fixed/continue.pty.raw
fixed/loopback.requests.jsonl
old-negative/old-final.screen.txt
old-negative/old.pty.raw
```

## Final source verification

The parent independently ran the following on final production revision `d960c8f`, using remote Rust 1.85.0 and locked/offline dependencies:

- `cargo fmt --all -- --check`: passed.
- `cargo test --all-targets`: 836 passed, 0 failed, 53 ignored.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps`: passed.
- Fixed-Agent E2E with `--ignored --test-threads=1 --nocapture`: 34 passed.
- Release performance with `--ignored --test-threads=1 --nocapture`: 9 passed.

Logs are under `/root/minicore-tui-v03-refactor/logs/display-bug/final-d960c8f-parent/`. An initial E2E invocation omitted CI's serial setting and had two failures (compaction state and editor timeout); its output is preserved as `e2e-nonstandard-parallel.log`. The subsequent CI-equivalent serial run passed without changing assertions. Earlier dual-toolchain results concern `311c5b3`, not the final production revision.

## Installed macOS artifact

The parent compared the downloaded SHA-256 with the exact remote artifact, checked `file`, `codesign --verify --strict`, and `--version`, then installed it at:

```text
/Users/zzq/Develops/minicore-tui/target/debug/minicore-tui
SHA-256: c57f97085d671c92013851443687aefa094ce34bdc0b95f1829372cd3f111aee
```

The exact remote artifact is `/root/minicore-tui-v03-refactor/macos-test-target-stateless-recovery/x86_64-apple-darwin/debug/minicore-tui`. It was cross-built with Rust 1.85.0, LLVM 19, `MacOSX.sdk`, deployment target 11.0, `CARGO_INCREMENTAL=0`, `-j6`, and locked/offline dependencies.

The installed executable was then exercised again through the same strict pyte harness on Intel macOS, with temporary workspace/data and the unchanged local Agent 0.5.0. First run and `--continue` exited 0; every message and tool result occurred exactly once in the expected order, and all four loopback responses completed. The old-control executable displayed the tool result but omitted the post-tool assistant text after completion and ready state.

Committed evidence from that installed-binary run:

- [Provenance](installed-PROVENANCE.json)
- [Streaming screen](installed-streaming.screen.txt)
- [Completed conversation](installed-final.screen.txt)
- [Reopened history](installed-continue.screen.txt)
- [Old-control screen](old-final.screen.txt)

Full raw PTY artifacts remain at `/tmp/minicore-tui-display-bug-macos-stateless-recovery/installed-evidence/`. The replaced executable is backed up at `/tmp/minicore-tui-display-bug-macos-stateless-recovery/minicore-tui-before-install`. No private configuration, user Store, Agent binary, or Runtime source was changed by the display fix.

## Boundaries

These are Linux and Intel macOS kernel-PTY checks using a loopback model, not external-provider validation, manual iTerm2/IME acceptance, a native macOS full Rust suite, Windows validation, or hosted CI. They do verify actual terminal screen state rather than merely received events or raw output substring presence.
