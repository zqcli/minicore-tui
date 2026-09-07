#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:/usr/local/bin:$PATH
root=/root/minicore-tui-027-RXdxEP
export CARGO_TARGET_DIR="$root/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/minicore-tui/scripts/macos-linker.sh"
export SDKROOT="$root/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
mkdir -p "$root/artifacts-llvm"
cd "$root/baseline-minicore-tui"
rustup run 1.98.0 cargo build --locked --offline --target x86_64-apple-darwin > "$root/logs/macos-llvm-baseline-debug.log" 2>&1
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/debug/minicore-tui" "$root/artifacts-llvm/minicore-tui-0.2.6-debug-macos"
cd "$root/minicore-tui"
rustup run 1.98.0 cargo build --locked --offline --target x86_64-apple-darwin > "$root/logs/macos-llvm-debug.log" 2>&1
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/debug/minicore-tui" "$root/artifacts-llvm/minicore-tui-0.2.7-debug-macos"
rustup run 1.98.0 cargo build --locked --offline --release --target x86_64-apple-darwin > "$root/logs/macos-llvm-release.log" 2>&1
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/minicore-tui" "$root/artifacts-llvm/minicore-tui-0.2.7-release-macos"
rustup run 1.98.0 cargo build --locked --offline --target x86_64-apple-darwin --tests --message-format=json > "$root/logs/macos-llvm-tests.jsonl" 2> "$root/logs/macos-llvm-tests.log"
python3 -c 'import json,pathlib,shutil; root=pathlib.Path("/root/minicore-tui-027-RXdxEP"); frames=[json.loads(line) for line in (root/"logs/macos-llvm-tests.jsonl").read_text().splitlines() if line.startswith("{")]; specs={"terminal_restore":"terminal-restore-0.2.7-macos", "minicore_tui":"minicore-tui-lib-tests-0.2.7-macos"}; [(shutil.copy2(next(f["executable"] for f in frames if f.get("reason")=="compiler-artifact" and f.get("target",{}).get("name")==name and f.get("executable") and f.get("profile",{}).get("test")),root/"artifacts-llvm"/output)) for name,output in specs.items()]'
/usr/lib/llvm-19/bin/dsymutil "$root/artifacts-llvm/minicore-tui-0.2.7-debug-macos" -o "$root/artifacts-llvm/minicore-tui-0.2.7-debug-macos.dSYM" > "$root/logs/dsym.log" 2>&1
sha256sum "$root"/artifacts-llvm/*macos > "$root/logs/llvm-artifact-hashes.txt"
printf 'ALL_LLVM_MACOS_BUILDS_PASS\n'
