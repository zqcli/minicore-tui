#!/bin/sh
set -eu

# A clang driver must recognize LLD by name to emit -platform_version.
# Point PATH at the LLVM bin directory containing clang and ld64.lld.
: "${SDKROOT:?SDKROOT must point to a macOS SDK}"
linker_dir=$(dirname "$(command -v ld64.lld)")
exec clang \
    -target x86_64-apple-macosx11.0 \
    -isysroot "$SDKROOT" \
    -B"$linker_dir" \
    -fuse-ld=lld \
    -Wl,-adhoc_codesign \
    "$@"
