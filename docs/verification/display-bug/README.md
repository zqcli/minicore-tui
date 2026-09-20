# Display Bug Verification

This evidence validates the async transcript-section fix from `b1bb062`, the initial tool-section deduplication fix, and the bounded tool-owner/live-transition fix from `311c5b3` through a real TUI, fixed Agent, deterministic loopback HTTP Responses model, Linux kernel PTYs, and `pyte` terminal parsing.

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
TUI:   /root/minicore-tui-v03-refactor/tui-target/display-bug-review-fixed/release/minicore-tui
OLD:   /root/minicore-tui-v03-refactor/tui-target/display-bug-old-7fea27e/debug/minicore-tui
Agent: /root/minicore-tui-v03-refactor/fixedagent-target/debug/minicore-agent
OUT:   /root/minicore-tui-v03-refactor/logs/display-bug/conversation-validation-311c5b3-final
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

The durable layout builds its ToolKey index once with the immutable `ConversationLayout`; cached live-tail composition borrows that index and does not scan transcript history. A partial Assistant ToolCall without a matching durable ToolBlock is treated as a history marker, not a complete tool owner, so the live tool card retains its current result/status until the ToolResult arrives. The focused regressions cover partial-history live state, live-to-durable replacement, nonzero viewport ordering, and cached ToolKey-index reuse.

## Accepted Remote Result

`PROVENANCE.json` records binary hashes and all assertions:

```text
/root/minicore-tui-v03-refactor/logs/display-bug/conversation-validation-311c5b3-final/PROVENANCE.json
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

## Boundaries

This is Linux kernel-PTY and loopback-provider evidence. It is not hosted CI, native macOS/Windows terminal execution, manual iTerm2/IME interaction, or external-provider validation. The macOS cross-built executable is validated separately for file integrity and isolated local execution; it is staged pending parent review and was not installed over `target/debug`.

The current staged macOS artifact is outside the repository at:

```text
/tmp/minicore-tui-display-bug-macos-311c5b3/minicore-tui
```

Its SHA-256 is `fb80dd1543124a41fcc354a778d84e8b40e751adb26251b88b65fcbf4570766e`; `file`, `codesign --verify --strict`, and x86_64 `--version` all passed. The local staged smoke used the existing x86_64 Agent binary with isolated temporary workspace/data and produced:

```text
/tmp/minicore-tui-display-bug-macos-311c5b3/evidence-final/PROVENANCE.json
```

The remote cross-build log is at `/root/minicore-tui-v03-refactor/logs/display-bug/review-311c5b3/macos-cross-build.log`. The remote build used Rust 1.85.0, LLVM 19, `MacOSX.sdk`, deployment target 11.0, `CARGO_INCREMENTAL=0`, `-j6`, and locked offline dependencies. The staged macOS old-control binary is `/tmp/minicore-tui-display-bug-macos-311c5b3/minicore-tui-old-7fea27e-macos` with SHA-256 `61da982dccb7c6a1ce8d2982659ded917678c109458e4e9aeb369f704797e027`.
