#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

for tool in rustup clang ld64.lld; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'error: %s is required (put the LLVM bin directory on PATH)\n' "$tool" >&2
        exit 1
    fi
done
if [ -z "${SDKROOT:-}" ] || [ ! -d "$SDKROOT/usr/lib" ]; then
    printf '%s\n' 'error: SDKROOT must point to a macOS SDK containing usr/lib' >&2
    exit 1
fi

readonly target=x86_64-apple-darwin
MACOSX_DEPLOYMENT_TARGET=11.0
export MACOSX_DEPLOYMENT_TARGET

unset \
    RUSTFLAGS \
    CARGO_ENCODED_RUSTFLAGS \
    CARGO_BUILD_RUSTFLAGS \
    CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS \
    CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER \
    CARGO_BUILD_TARGET \
    CARGO_TARGET_DIR \
    CARGO_BUILD_TARGET_DIR

# Zig 0.13-linked Rust binaries failed even a standalone catch_unwind probe.
# Use LLVM's Mach-O linker, and keep native panic restoration in acceptance.
export CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER="$script_dir/macos-linker.sh"
rustup run 1.85.0 cargo build --release --locked --target "$target"
test -x target/x86_64-apple-darwin/release/minicore-tui
artifact=target/$target/release/minicore-tui
printf 'artifact=%s\n' "$artifact"
printf 'artifact_absolute=%s/%s\n' "$repo_root" "$artifact"
