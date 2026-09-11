#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:$PATH
root=/root/minicore-tui-stream-smooth.z2MHSJ
cache=/root/minicore-tui-027-RXdxEP
export CARGO_TARGET_DIR="$cache/target-linux"
cd "$root/minicore-agent"
rustup run stable cargo build -j 4 --locked --offline > "$root/logs/final-agent-linux-build.log" 2>&1
cd "$root/minicore-tui"
MINICORE_AGENT_BIN="$cache/target-linux/debug/minicore-agent" rustup run stable cargo test -j 4 --locked --offline --test agent_e2e -- --ignored --test-threads=1 > "$root/logs/final-e2e.log" 2>&1
RUSTDOCFLAGS='-D warnings' rustup run stable cargo doc -j 4 --locked --offline --no-deps > "$root/logs/final-doc.log" 2>&1
export CARGO_TARGET_DIR="$cache/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/baseline-tui/scripts/macos-linker.sh"
export SDKROOT="$cache/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
export RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x4000 -C link-arg=-mmacosx-version-min=11.0'
for profile in debug release; do
  flags=()
  if [[ "$profile" == release ]]; then flags+=(--release); fi
  rustup run 1.98.0 cargo build -j 4 --locked --offline --target x86_64-apple-darwin "${flags[@]}" > "$root/logs/final-macos-$profile.log" 2>&1
  cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/$profile/minicore-tui" "$root/artifacts/final-tui-$profile"
done
dsymutil "$root/artifacts/final-tui-debug" -o "$root/artifacts/final-tui-debug.dSYM" > "$root/logs/final-dsym.log" 2>&1
rustup run 1.98.0 cargo build -j 4 --locked --offline --target x86_64-apple-darwin --test terminal_restore --message-format=json > "$root/logs/final-tty-build.jsonl" 2> "$root/logs/final-tty-build.log"
python3 -c 'import json,pathlib,shutil; r=pathlib.Path("/root/minicore-tui-stream-smooth.z2MHSJ"); frames=[json.loads(l) for l in (r/"logs/final-tty-build.jsonl").read_text().splitlines() if l.startswith("{")]; shutil.copy2(next(f["executable"] for f in frames if f.get("reason")=="compiler-artifact" and f.get("target",{}).get("name")=="terminal_restore" and f.get("executable")),r/"artifacts/terminal-restore")'
sha256sum "$root/artifacts/"*-debug "$root/artifacts/"*-release "$root/artifacts/terminal-restore" > "$root/logs/final-hashes.txt"
printf 'FINAL_REMOTE_E2E_MACOS_PASS\n'
