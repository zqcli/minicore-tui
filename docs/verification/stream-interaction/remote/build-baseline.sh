#!/usr/bin/env bash
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:$PATH
root=/root/minicore-tui-stream-smooth.z2MHSJ
cache=/root/minicore-tui-027-RXdxEP
export CARGO_TARGET_DIR="$cache/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/baseline-tui/scripts/macos-linker.sh"
export SDKROOT="$cache/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
export CC_x86_64_apple_darwin=clang AR_x86_64_apple_darwin=llvm-ar
export CFLAGS_x86_64_apple_darwin="-target x86_64-apple-macosx11.0 -isysroot $SDKROOT"
export RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x4000 -C link-arg=-mmacosx-version-min=11.0'
cd "$root/baseline-tui"
for profile in debug release; do
  flags=()
  if [[ "$profile" == release ]]; then flags+=(--release); fi
  rustup run 1.98.0 cargo build -j 4 --locked --offline --target x86_64-apple-darwin "${flags[@]}" > "$root/logs/baseline-macos-$profile.log" 2>&1
  cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/$profile/minicore-tui" "$root/artifacts/baseline-tui-$profile"
done
cd "$root/minicore-agent"
rustup run 1.98.0 cargo build -j 4 --locked --offline --target x86_64-apple-darwin > "$root/logs/agent-macos-debug.log" 2>&1
cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/debug/minicore-agent" "$root/artifacts/agent-debug"
sha256sum "$root/artifacts/"* > "$root/logs/baseline-hashes.txt"
printf 'BASELINE_MACOS_PASS\n'
