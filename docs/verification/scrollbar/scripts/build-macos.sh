#!/usr/bin/env bash
set -euo pipefail
[[ $(uname -s) == Linux ]] || exit 2
root=/root/minicore-scrollbar.1ulnCV
cache=/root/minicore-tui-027-RXdxEP
export PATH=/root/.cargo/bin:/usr/lib/llvm-19/bin:$PATH
export CARGO_TARGET_DIR="$cache/target-macos-llvm"
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$root/scripts/macos-linker.sh"
export SDKROOT="$cache/MacOSX.sdk" MACOSX_DEPLOYMENT_TARGET=11.0
export CC_x86_64_apple_darwin=clang AR_x86_64_apple_darwin=llvm-ar
export CFLAGS_x86_64_apple_darwin="-target x86_64-apple-macosx11.0 -isysroot $SDKROOT"
export RUSTFLAGS='-C link-arg=-Wl,-headerpad,0x4000 -C link-arg=-mmacosx-version-min=11.0'
cd "$root"
{ rustup run stable rustc --version -v; rustup run 1.85.0 rustc --version; rustup run 1.98.0 rustc --version; clang --version; uname -a; df -Pk "$root"; } > "$root/logs/toolchains.log"
for profile in debug release; do
 flags=(); if [[ $profile == release ]]; then flags+=(--release); fi
 rustup run 1.98.0 cargo build -j4 --locked --offline --target x86_64-apple-darwin "${flags[@]}" > "$root/logs/macos-$profile.log" 2>&1
 cp "$CARGO_TARGET_DIR/x86_64-apple-darwin/$profile/minicore-tui" "$root/artifacts/minicore-tui-$profile"
done
dsymutil "$root/artifacts/minicore-tui-debug" -o "$root/artifacts/minicore-tui-debug.dSYM" > "$root/logs/dsym.log" 2>&1
(cd "$root/artifacts" && sha256sum minicore-tui-debug minicore-tui-release minicore-tui-debug.dSYM/Contents/Resources/DWARF/minicore-tui-debug) > "$root/logs/macos-hashes.log"
printf 'SCROLLBAR_MACOS_BUILD_PASS\n'
