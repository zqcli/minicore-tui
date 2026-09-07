#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:/usr/local/bin:$PATH
root=/root/minicore-tui-027-RXdxEP
export CARGO_TARGET_DIR="$root/target-macos-opt1"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/minicore-tui/scripts/macos-linker.sh"
export SDKROOT="$root/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
mkdir -p "$root/artifacts-opt1"
cd "$root/baseline-minicore-tui"
rustup run 1.98.0 cargo build --locked --offline --config profile.dev.opt-level=1 --target x86_64-apple-darwin > "$root/logs/opt1-baseline.log" 2>&1
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/debug/minicore-tui" "$root/artifacts-opt1/minicore-tui-baseline-opt1-macos"
cd "$root/minicore-tui"
rustup run 1.98.0 cargo build --locked --offline --config profile.dev.opt-level=1 --target x86_64-apple-darwin > "$root/logs/opt1-buffered.log" 2>&1
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/debug/minicore-tui" "$root/artifacts-opt1/minicore-tui-buffered-opt1-macos"
printf 'OPT1_BUILDS_PASS\n'
